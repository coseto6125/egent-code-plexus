//! Vue SFC parser — two-pass: tree-sitter-vue for block structure,
//! tree-sitter-typescript/javascript for embedded `<script>` content.
//!
//! ## LLM-utility justification (CLAUDE.md §B — Node coverage)
//!
//! Without this parser, `<script>` and `<script setup>` block contents are
//! invisible to the graph. Agents querying `ecp impact` on a function
//! defined in `<script setup>` get zero callers or wrong results. This
//! parser makes those symbols first-class graph nodes with correct file:line
//! spans, enabling accurate cross-file impact analysis.
//!
//! ## Two-pass strategy
//!
//! Pass 1 (Vue grammar): locate `<script>`, `<template>`, `<style>` block
//! positions and read the `lang=` / `setup` attributes from each `start_tag`.
//!
//! Pass 2 (TS/JS grammar): re-parse the raw byte slice of each script block
//! using tree-sitter-typescript (default) or tree-sitter-javascript (`lang="js"`).
//! All span rows emitted by the inner parse are offset by the script block's
//! start row so they reference the .vue file's line numbers, not the script-local
//! line numbers.
//!
//! `<template>` and `<style>` blocks: emit a `Section` RawNode for the block
//! span only; contents are not parsed (no LLM-utility without template AST
//! cross-linking, which is deferred).

use crate::framework_helpers::node_span;
use crate::parse_budget::{parse_with_budget, ParseBudget};
use crate::sfc_common;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

pub struct VueProvider {
    vue_language: Language,
    ts_language: Language,
    js_language: Language,
    vue_query: Query,
    ts_query: Query,
    js_query: Query,
    ts_capture_by_idx: Vec<Option<NodeKind>>,
    js_capture_by_idx: Vec<Option<NodeKind>>,
    ts_root_span_mask: u64,
    js_root_span_mask: u64,
}

impl VueProvider {
    pub fn new() -> anyhow::Result<Self> {
        let vue_language: Language = tree_sitter_vue::LANGUAGE.into();
        let vue_query = Query::new(&vue_language, include_str!("queries.scm"))?;

        let ts_language: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let ts_query = Query::new(&ts_language, include_str!("../typescript/queries.scm"))?;
        let ts_capture_by_idx = sfc_common::capture_kind_by_idx(&ts_query);
        let ts_root_span_mask = sfc_common::root_span_mask(&ts_query);

        let js_language: Language = tree_sitter_javascript::LANGUAGE.into();
        let js_query = Query::new(&js_language, include_str!("../javascript/queries.scm"))?;
        let js_capture_by_idx = sfc_common::capture_kind_by_idx(&js_query);
        let js_root_span_mask = sfc_common::root_span_mask(&js_query);

        Ok(Self {
            vue_language,
            ts_language,
            js_language,
            vue_query,
            ts_query,
            js_query,
            ts_capture_by_idx,
            js_capture_by_idx,
            ts_root_span_mask,
            js_root_span_mask,
        })
    }
}

impl LanguageProvider for VueProvider {
    fn name(&self) -> &'static str {
        "vue"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        // ── Pass 1: Vue SFC structure ─────────────────────────────────────
        let mut vue_parser = Parser::new();
        vue_parser.set_language(&self.vue_language)?;

        let vue_tree = parse_with_budget(&mut vue_parser, source, ParseBudget::DEFAULT)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter-vue failed to parse {:?}", path))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.vue_query, vue_tree.root_node(), source);

        let idx_script = self.vue_query.capture_index_for_name("script");
        let idx_script_tag = self.vue_query.capture_index_for_name("script.tag");
        let idx_script_body = self.vue_query.capture_index_for_name("script.body");
        let idx_template = self.vue_query.capture_index_for_name("template");
        let idx_style = self.vue_query.capture_index_for_name("style");

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
            let mut template_elem: Option<tree_sitter::Node> = None;
            let mut style_elem: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let ci = Some(cap.index);
                if ci == idx_script {
                    script_elem = Some(cap.node);
                } else if ci == idx_script_tag {
                    script_tag = Some(cap.node);
                } else if ci == idx_script_body {
                    script_body = Some(cap.node);
                } else if ci == idx_template {
                    template_elem = Some(cap.node);
                } else if ci == idx_style {
                    style_elem = Some(cap.node);
                }
            }

            if let (Some(elem), Some(tag)) = (script_elem, script_tag) {
                script_blocks.push((elem, tag, script_body));
            }

            if let Some(node) = template_elem {
                section_nodes.push(RawNode {
                    name: "template".to_string(),
                    kind: NodeKind::Section,
                    span: node_span(&node),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: 0,
                });
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
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: 0,
                });
            }
        }

        // ── Pass 2: re-parse each script block ───────────────────────────
        let mut all_nodes: Vec<RawNode> = section_nodes;
        let mut all_imports: Vec<RawImport> = Vec::new();

        for (script_elem, start_tag, raw_text_node) in script_blocks {
            // Determine `lang` attribute (default: "ts" for Vue 3 SFCs,
            // though technically the spec default is JS. We keep TypeScript
            // as the safe default because Vue 3 + TS is the dominant setup;
            // callers should set `lang="js"` explicitly for JS-only files).
            let is_ts = detect_lang_ts(&start_tag, source);
            let is_setup = detect_setup_attr(&start_tag, source);
            let section_name = if is_setup { "script setup" } else { "script" };

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
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
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
/// Vue 3 + TypeScript is the dominant setup, and TS parsing is a safe
/// superset for JS content in tree-sitter-typescript.
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
        // Found `lang` attribute — look at its value.
        let val_node = child.child_by_field_name("attribute_value").or_else(|| {
            // quoted_attribute_value wraps the inner string.
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
        // `lang` present but no readable value — default TS.
        return true;
    }
    // No `lang` attribute — default TypeScript (Vue 3 idiomatic).
    true
}

/// Returns `true` when `<script setup>` — i.e. the start_tag has a bare
/// `setup` attribute (no value) or `setup=""`.
fn detect_setup_attr(start_tag: &tree_sitter::Node, source: &[u8]) -> bool {
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
        if name_str == "setup" {
            return true;
        }
    }
    false
}
