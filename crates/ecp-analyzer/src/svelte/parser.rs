//! Svelte SFC parser — two-pass: tree-sitter-svelte for block structure,
//! tree-sitter-typescript/javascript for embedded `<script>` content.
//!
//! ## LLM-utility justification (CLAUDE.md §B — Node coverage)
//!
//! Without this parser, `<script>` block contents are invisible to the graph.
//! Agents querying `ecp impact` on a function defined in a Svelte component
//! get zero callers or wrong results. This parser makes those symbols
//! first-class graph nodes with correct file:line spans, enabling accurate
//! cross-file impact analysis. gitnexus has no Svelte support; this is an
//! ecp-leading feature.
//!
//! ## Two-pass strategy
//!
//! Pass 1 (Svelte grammar): locate `<script>` and `<style>` block positions
//! and read the `lang=` / `context=` attributes from each `start_tag`.
//!
//! Pass 2 (TS/JS grammar): re-parse the raw byte slice of each script block
//! using tree-sitter-typescript (default) or tree-sitter-javascript (`lang="js"`).
//! All span rows emitted by the inner parse are offset by the script block's
//! start row so they reference the .svelte file's line numbers, not the
//! script-local line numbers.
//!
//! `<style>` blocks: emit a `Section` RawNode for the block span only;
//! contents are not parsed (no LLM-utility without CSS cross-linking).
//!
//! The top-level Svelte HTML template (everything that is not `<script>` or
//! `<style>`) is implicitly covered by file-level span; no explicit template
//! Section is emitted because the Svelte grammar has no single wrapping
//! template node — the document root IS the template.
//!
//! ## Svelte 5 runes
//!
//! `$state()`, `$derived()`, `$effect()`, `$props()` are Svelte 5 compiler
//! hints that appear in `<script>` blocks as syntactically ordinary call
//! expressions. The tree-sitter grammar does not assign them special node
//! types. The TypeScript/JavaScript pass will capture the LHS variable via
//! the normal `variable` / `const` capture patterns, and the rune call itself
//! may surface as a `Calls` edge if the TS/JS queries capture call_expressions.
//! Full semantic awareness of rune semantics (reactivity graph, derived
//! dependency tracking) requires Svelte compiler integration and is out of
//! scope for this PR.
//!
//! ## Module-scope script
//!
//! `<script context="module">` declares module-level code that runs once
//! when the module is first imported, separate from the per-instance
//! `<script>` block. Both blocks are parsed independently; the module-scope
//! block emits a `Section` named `"script module"`.

use crate::framework_helpers::node_span;
use crate::parse_budget::{parse_with_budget, ParseBudget};
use crate::sfc_common;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

pub struct SvelteProvider {
    svelte_language: Language,
    ts_language: Language,
    js_language: Language,
    svelte_query: Query,
    ts_query: Query,
    js_query: Query,
    ts_capture_by_idx: Vec<Option<NodeKind>>,
    js_capture_by_idx: Vec<Option<NodeKind>>,
    ts_root_span_mask: u64,
    js_root_span_mask: u64,
}

impl SvelteProvider {
    pub fn new() -> anyhow::Result<Self> {
        let svelte_language: Language = tree_sitter_svelte::LANGUAGE.into();
        let svelte_query = Query::new(&svelte_language, include_str!("queries.scm"))?;

        let ts_language: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let ts_query = Query::new(&ts_language, include_str!("../typescript/queries.scm"))?;
        let ts_capture_by_idx = sfc_common::capture_kind_by_idx(&ts_query);
        let ts_root_span_mask = sfc_common::root_span_mask(&ts_query);

        let js_language: Language = tree_sitter_javascript::LANGUAGE.into();
        let js_query = Query::new(&js_language, include_str!("../javascript/queries.scm"))?;
        let js_capture_by_idx = sfc_common::capture_kind_by_idx(&js_query);
        let js_root_span_mask = sfc_common::root_span_mask(&js_query);

        Ok(Self {
            svelte_language,
            ts_language,
            js_language,
            svelte_query,
            ts_query,
            js_query,
            ts_capture_by_idx,
            js_capture_by_idx,
            ts_root_span_mask,
            js_root_span_mask,
        })
    }
}

impl LanguageProvider for SvelteProvider {
    fn name(&self) -> &'static str {
        "svelte"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        // ── Pass 1: Svelte SFC structure ──────────────────────────────────
        let mut svelte_parser = Parser::new();
        svelte_parser.set_language(&self.svelte_language)?;

        let svelte_tree = parse_with_budget(&mut svelte_parser, source, ParseBudget::DEFAULT)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter-svelte failed to parse {:?}", path))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.svelte_query, svelte_tree.root_node(), source);

        let idx_script = self.svelte_query.capture_index_for_name("script");
        let idx_script_tag = self.svelte_query.capture_index_for_name("script.tag");
        let idx_script_body = self.svelte_query.capture_index_for_name("script.body");
        let idx_style = self.svelte_query.capture_index_for_name("style");

        let mut section_nodes: Vec<RawNode> = Vec::new();
        // (script_element_node, start_tag_node, Option<raw_text_node>)
        let mut script_blocks: Vec<(
            tree_sitter::Node,
            tree_sitter::Node,
            Option<tree_sitter::Node>,
        )> = Vec::new();

        while let Some(m) = matches.next() {
            let mut script_elem: Option<tree_sitter::Node> = None;
            let mut script_tag: Option<tree_sitter::Node> = None;
            let mut script_body: Option<tree_sitter::Node> = None;
            let mut style_elem: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let ci = Some(cap.index);
                if ci == idx_script {
                    script_elem = Some(cap.node);
                } else if ci == idx_script_tag {
                    script_tag = Some(cap.node);
                } else if ci == idx_script_body {
                    script_body = Some(cap.node);
                } else if ci == idx_style {
                    style_elem = Some(cap.node);
                }
            }

            if let (Some(elem), Some(tag)) = (script_elem, script_tag) {
                script_blocks.push((elem, tag, script_body));
            }

            if let Some(node) = style_elem {
                section_nodes.push(RawNode {
                    name: "style".to_string(),
                    kind: NodeKind::Section,
                    span: node_span(&node),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                });
            }
        }

        // ── Pass 2: re-parse each script block ───────────────────────────
        let mut all_nodes: Vec<RawNode> = section_nodes;
        let mut all_imports: Vec<RawImport> = Vec::new();

        for (script_elem, start_tag, raw_text_node) in script_blocks {
            let is_ts = detect_lang_ts(&start_tag, source);
            let is_module = detect_context_module(&start_tag, source);
            let section_name = if is_module { "script module" } else { "script" };

            // Emit Section node for the script block itself.
            all_nodes.push(RawNode {
                name: section_name.to_string(),
                kind: NodeKind::Section,
                span: node_span(&script_elem),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
            });

            let body_node = match raw_text_node {
                Some(n) => n,
                // Empty <script></script> — nothing to parse.
                None => continue,
            };

            let script_start_row = body_node.start_position().row as u32;
            let script_start_col = body_node.start_position().column as u32;
            let body_start = body_node.start_byte();
            let body_end = body_node.end_byte();
            let script_source = &source[body_start..body_end];

            let (nodes, imports) = if is_ts {
                sfc_common::parse_embedded_script(
                    script_source,
                    &self.ts_query,
                    &self.ts_capture_by_idx,
                    self.ts_root_span_mask,
                    &self.ts_language,
                    script_start_row,
                    script_start_col,
                )
            } else {
                sfc_common::parse_embedded_script(
                    script_source,
                    &self.js_query,
                    &self.js_capture_by_idx,
                    self.js_root_span_mask,
                    &self.js_language,
                    script_start_row,
                    script_start_col,
                )
            };

            all_nodes.extend(nodes);
            all_imports.extend(imports);
        }

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            nodes: all_nodes,
            imports: all_imports,
            ..Default::default()
        })
    }
}

/// Returns `true` when the `<script>` tag has `lang="ts"` attribute.
/// Falls back to `true` (TypeScript) when no `lang` attribute is present —
/// TypeScript parsing is a safe superset for JS content in tree-sitter-typescript,
/// and Svelte + TypeScript is the dominant setup.
fn detect_lang_ts(start_tag: &tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = start_tag.walk();
    for child in start_tag.children(&mut cursor) {
        if child.kind() != "attribute" {
            continue;
        }
        let attr_name = child
            .child_by_field_name("attribute_name")
            .or_else(|| child.named_child(0));
        let Some(name_node) = attr_name else { continue };
        let Ok(name_str) =
            std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
        else {
            continue;
        };
        if name_str != "lang" {
            continue;
        }
        // Found `lang` attribute — check its value.
        let val_node = child.child_by_field_name("attribute_value").or_else(|| {
            child
                .named_children(&mut child.walk())
                .find(|n| n.kind() == "attribute_value" || n.kind() == "quoted_attribute_value")
        });
        if let Some(v) = val_node {
            let raw = &source[v.start_byte()..v.end_byte()];
            let val = std::str::from_utf8(raw)
                .unwrap_or("")
                .trim_matches(|c| c == '"' || c == '\'');
            return matches!(val, "ts" | "typescript");
        }
        return true;
    }
    // No `lang` attribute — default TypeScript.
    true
}

/// Returns `true` when `<script context="module">` — i.e. the start_tag has
/// a `context` attribute with value `"module"`.
fn detect_context_module(start_tag: &tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = start_tag.walk();
    for child in start_tag.children(&mut cursor) {
        if child.kind() != "attribute" {
            continue;
        }
        let attr_name = child
            .child_by_field_name("attribute_name")
            .or_else(|| child.named_child(0));
        let Some(name_node) = attr_name else { continue };
        let Ok(name_str) =
            std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
        else {
            continue;
        };
        if name_str != "context" {
            continue;
        }
        // Found `context` attribute — check it equals "module".
        let val_node = child.child_by_field_name("attribute_value").or_else(|| {
            child
                .named_children(&mut child.walk())
                .find(|n| n.kind() == "attribute_value" || n.kind() == "quoted_attribute_value")
        });
        if let Some(v) = val_node {
            let raw = &source[v.start_byte()..v.end_byte()];
            let val = std::str::from_utf8(raw)
                .unwrap_or("")
                .trim_matches(|c| c == '"' || c == '\'');
            return val == "module";
        }
    }
    false
}
