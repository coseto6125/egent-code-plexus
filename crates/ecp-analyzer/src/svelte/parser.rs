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
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// Span type alias matching `framework_helpers::node_span` return.
type Span = (u32, u32, u32, u32);

pub struct SvelteProvider {
    svelte_query: Query,
    ts_query: Query,
    js_query: Query,
    ts_capture_by_idx: Vec<Option<NodeKind>>,
    js_capture_by_idx: Vec<Option<NodeKind>>,
}

/// Pre-resolved capture name → NodeKind table for both TS and JS grammars.
fn capture_kind(name: &str) -> Option<NodeKind> {
    match name {
        // TypeScript-style captures
        "function.name" => Some(NodeKind::Function),
        "class.name" => Some(NodeKind::Class),
        "method.name" => Some(NodeKind::Method),
        "constructor.name" => Some(NodeKind::Constructor),
        "interface.name" => Some(NodeKind::Interface),
        "typedef.name" => Some(NodeKind::Typedef),
        "property.name" => Some(NodeKind::Property),
        "const.name" => Some(NodeKind::Const),
        "variable.name" => Some(NodeKind::Variable),
        "enum.name" => Some(NodeKind::Enum),
        // JavaScript-style captures (name.<kind> convention)
        "name.function" => Some(NodeKind::Function),
        "name.class" => Some(NodeKind::Class),
        "name.method" => Some(NodeKind::Method),
        _ => None,
    }
}

impl SvelteProvider {
    pub fn new() -> anyhow::Result<Self> {
        let svelte_language = tree_sitter_svelte::LANGUAGE.into();
        let svelte_query = Query::new(&svelte_language, include_str!("queries.scm"))?;

        let ts_language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let ts_query_src = include_str!("../typescript/queries.scm");
        let ts_query = Query::new(&ts_language, ts_query_src)?;
        let ts_capture_by_idx = ts_query
            .capture_names()
            .iter()
            .map(|n| capture_kind(n))
            .collect();

        let js_language = tree_sitter_javascript::LANGUAGE.into();
        let js_query_src = include_str!("../javascript/queries.scm");
        let js_query = Query::new(&js_language, js_query_src)?;
        let js_capture_by_idx = js_query
            .capture_names()
            .iter()
            .map(|n| capture_kind(n))
            .collect();

        Ok(Self {
            svelte_query,
            ts_query,
            js_query,
            ts_capture_by_idx,
            js_capture_by_idx,
        })
    }
}

impl LanguageProvider for SvelteProvider {
    fn name(&self) -> &'static str {
        "svelte"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        // ── Pass 1: Svelte SFC structure ──────────────────────────────────
        let svelte_language = tree_sitter_svelte::LANGUAGE.into();
        let mut svelte_parser = Parser::new();
        svelte_parser.set_language(&svelte_language)?;

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
                parse_script_content(
                    script_source,
                    &self.ts_query,
                    &self.ts_capture_by_idx,
                    || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                    script_start_row,
                    script_start_col,
                )
            } else {
                parse_script_content(
                    script_source,
                    &self.js_query,
                    &self.js_capture_by_idx,
                    || tree_sitter_javascript::LANGUAGE.into(),
                    script_start_row,
                    script_start_col,
                )
            };

            all_nodes.extend(nodes);
            all_imports.extend(imports);
        }

        Ok(LocalGraph {
            content_hash: [0; 8],
            file_path: path.to_path_buf(),
            nodes: all_nodes,
            imports: all_imports,
            routes: vec![],
            documents: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
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

/// Parse a script block's raw source with the given `query` + `grammar`,
/// returning `(nodes, imports)` with all span rows offset by `row_offset`
/// so spans reference positions in the containing .svelte file.
fn parse_script_content(
    script_source: &[u8],
    query: &Query,
    capture_kind_by_idx: &[Option<NodeKind>],
    make_language: impl Fn() -> tree_sitter::Language,
    row_offset: u32,
    col_offset: u32,
) -> (Vec<RawNode>, Vec<RawImport>) {
    let language = make_language();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return (vec![], vec![]);
    }
    let Some(tree) = parse_with_budget(&mut parser, script_source, ParseBudget::DEFAULT) else {
        return (vec![], vec![]);
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), script_source);

    let idx_import = query.capture_index_for_name("import");
    let idx_import_name = query.capture_index_for_name("import.name");
    let idx_import_alias = query.capture_index_for_name("import.alias");
    let idx_import_source = query.capture_index_for_name("import.source");
    let idx_import_ns = query.capture_index_for_name("import.namespace");

    let root_span_indices: Vec<u32> = query
        .capture_names()
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            matches!(
                **name,
                "function"
                    | "class"
                    | "method"
                    | "constructor"
                    | "interface"
                    | "property"
                    | "const"
                    | "variable"
                    | "typedef"
                    | "enum"
            )
        })
        .map(|(i, _)| i as u32)
        .collect();

    let export_idx = query.capture_index_for_name("export");

    let mut nodes: Vec<RawNode> = Vec::new();
    let mut imports: Vec<RawImport> = Vec::new();

    while let Some(m) = matches.next() {
        let mut name_node: Option<tree_sitter::Node> = None;
        let mut kind: Option<NodeKind> = None;
        let mut root_span_node: Option<tree_sitter::Node> = None;
        let mut is_exported = false;

        let mut import_name_node: Option<tree_sitter::Node> = None;
        let mut import_alias_node: Option<tree_sitter::Node> = None;
        let mut import_src_node: Option<tree_sitter::Node> = None;
        let mut is_import = false;
        let mut is_import_ns = false;

        for cap in m.captures {
            let ci = cap.index;
            if let Some(Some(k)) = capture_kind_by_idx.get(ci as usize) {
                name_node = Some(cap.node);
                kind = Some(*k);
            } else if Some(ci) == export_idx {
                is_exported = true;
            } else if Some(ci) == idx_import_name {
                import_name_node = Some(cap.node);
            } else if Some(ci) == idx_import_alias {
                import_alias_node = Some(cap.node);
            } else if Some(ci) == idx_import_source {
                import_src_node = Some(cap.node);
            } else if Some(ci) == idx_import {
                is_import = true;
            } else if Some(ci) == idx_import_ns {
                is_import_ns = true;
            } else if root_span_indices.contains(&ci) {
                root_span_node = Some(cap.node);
            }
        }

        if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
            if let Ok(name_str) = std::str::from_utf8(&script_source[n.start_byte()..n.end_byte()])
            {
                let raw_span = node_span(&root);
                let span = offset_span(raw_span, row_offset, col_offset);
                let mut existing = false;
                for node in &mut nodes {
                    if node.span == span && node.name == name_str {
                        if k == NodeKind::Function
                            && matches!(node.kind, NodeKind::Const | NodeKind::Variable)
                        {
                            node.kind = NodeKind::Function;
                        }
                        if is_exported {
                            node.is_exported = true;
                        }
                        existing = true;
                        break;
                    }
                }
                if !existing {
                    nodes.push(RawNode {
                        name: name_str.to_string(),
                        kind: k,
                        span,
                        is_exported,
                        heritage: vec![],
                        type_annotation: None,
                        decorators: vec![],
                        calls: vec![],
                    });
                }
            }
        }

        if is_import {
            if let (Some(i_name), Some(i_src)) = (import_name_node, import_src_node) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&script_source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&script_source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    let alias_str = import_alias_node.and_then(|a| {
                        std::str::from_utf8(&script_source[a.start_byte()..a.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });
                    imports.push(RawImport {
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                        alias: alias_str,
                        binding_kind: None,
                    });
                }
            }
        }

        if is_import_ns {
            if let (Some(a), Some(i_src)) = (import_alias_node, import_src_node) {
                if let (Ok(alias_str), Ok(src_str)) = (
                    std::str::from_utf8(&script_source[a.start_byte()..a.end_byte()]),
                    std::str::from_utf8(&script_source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    imports.push(RawImport {
                        imported_name: "*".to_string(),
                        source: src_str.to_string(),
                        alias: Some(alias_str.to_string()),
                        binding_kind: None,
                    });
                }
            }
        }
    }

    (nodes, imports)
}

/// Add `row_offset` to the start and end rows of a span, leaving columns intact.
/// This remaps script-local line numbers to .svelte file line numbers.
#[inline]
fn offset_span(span: Span, row_offset: u32, col_offset: u32) -> Span {
    let start_col = if span.0 == 0 {
        span.1.saturating_add(col_offset)
    } else {
        span.1
    };
    let end_col = if span.2 == 0 {
        span.3.saturating_add(col_offset)
    } else {
        span.3
    };
    (
        span.0.saturating_add(row_offset),
        start_col,
        span.2.saturating_add(row_offset),
        end_col,
    )
}
