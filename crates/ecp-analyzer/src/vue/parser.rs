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
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// Span type alias matching `framework_helpers::node_span` return.
type Span = (u32, u32, u32, u32);

pub struct VueProvider {
    vue_query: Query,
    ts_query: Query,
    js_query: Query,
    ts_capture_by_idx: Vec<Option<NodeKind>>,
    js_capture_by_idx: Vec<Option<NodeKind>>,
}

/// Pre-resolved capture name → NodeKind table for both TS and JS grammars.
///
/// TypeScript captures use `<kind>.name` convention (e.g. `function.name`).
/// JavaScript captures use `name.<kind>` convention (e.g. `name.function`).
/// Both are mapped here so the same `parse_script_content` helper works for
/// either grammar without specialisation.
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

impl VueProvider {
    pub fn new() -> anyhow::Result<Self> {
        let vue_language = tree_sitter_vue::LANGUAGE.into();
        let vue_query = Query::new(&vue_language, include_str!("queries.scm"))?;

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
            vue_query,
            ts_query,
            js_query,
            ts_capture_by_idx,
            js_capture_by_idx,
        })
    }
}

impl LanguageProvider for VueProvider {
    fn name(&self) -> &'static str {
        "vue"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        // ── Pass 1: Vue SFC structure ─────────────────────────────────────
        let vue_language = tree_sitter_vue::LANGUAGE.into();
        let mut vue_parser = Parser::new();
        vue_parser.set_language(&vue_language)?;

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

/// Parse a script block's raw source with the given `query` + `grammar`,
/// returning `(nodes, imports)` with all span rows offset by `row_offset`
/// so spans reference positions in the containing .vue file.
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

    // Pre-resolve capture indices for import handling (same names as TS/JS parsers).
    let idx_import = query.capture_index_for_name("import");
    let idx_import_name = query.capture_index_for_name("import.name");
    let idx_import_alias = query.capture_index_for_name("import.alias");
    let idx_import_source = query.capture_index_for_name("import.source");
    let idx_import_ns = query.capture_index_for_name("import.namespace");

    // Span-root anchors — capture names that carry the declaration node span
    // (not the name identifier). TS uses e.g. `@function` / `@class`; JS uses
    // the same names for its span-carrying captures.
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
/// This remaps script-local line numbers to .vue file line numbers.
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
