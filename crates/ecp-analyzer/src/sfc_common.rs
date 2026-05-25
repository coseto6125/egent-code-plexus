//! Shared helpers for Vue / Astro / Svelte SFC parsers.
//!
//! All three follow the same shape — an outer grammar (vue/astro/svelte)
//! locates script or frontmatter blocks, then each block is re-parsed with
//! tree-sitter-typescript or tree-sitter-javascript. The inner parse logic
//! (capture dispatch, span remap, import + node emission) is identical
//! across the three providers, so it lives here once.

use crate::framework_helpers::{node_span, Span};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::types::{RawImport, RawNode};
use ecp_core::graph::NodeKind;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

/// Map a tree-sitter capture name to a `NodeKind` for SFC embedded
/// scripts. Handles both the TS convention (`<kind>.name`) and the JS
/// convention (`name.<kind>`); a single dispatch table covers either
/// grammar.
///
/// **Why this is not `TypeScriptSpec::CAPTURE_KIND ∪ JavaScriptSpec::CAPTURE_KIND`:**
/// the JS spec table intentionally omits `variable.name` because the
/// standalone JS parser runs an arrow-function / `const|let|var` dedup
/// pass via `idx_variable_name` *before* the spec lookup (see
/// `javascript/spec.rs:7-13`). SFC has no such pre-pass, so a JS query
/// capture of `@variable.name` here must resolve to `NodeKind::Variable`
/// directly — which the spec table won't do. Keep this list aligned
/// with the union of both query files' captures, not the spec tables.
pub(crate) fn capture_kind(name: &str) -> Option<NodeKind> {
    match name {
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
        "name.function" => Some(NodeKind::Function),
        "name.class" => Some(NodeKind::Class),
        "name.method" => Some(NodeKind::Method),
        _ => None,
    }
}

/// Build `capture_kind_by_idx` for `query` — call once at provider
/// construction. The result is indexed by capture index in the inner loop.
pub fn capture_kind_by_idx(query: &Query) -> Vec<Option<NodeKind>> {
    query
        .capture_names()
        .iter()
        .map(|n| capture_kind(n))
        .collect()
}

/// Build a bitmask of capture indices whose nodes carry the enclosing
/// declaration span (the "root span" for a function/class/method/...).
/// Used to distinguish a name-identifier capture from its enclosing node.
///
/// A `u64` mask is enough — tree-sitter queries in this crate have well
/// under 64 captures (TS query has ~25, JS query has ~10). The inner
/// match loop checks membership with a single bit-test instead of an
/// `O(K)` linear scan over a `Vec<u32>`.
pub fn root_span_mask(query: &Query) -> u64 {
    query
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
        .fold(0u64, |acc, (i, _)| acc | (1u64 << i))
}

/// Remap a script-local span into outer-file coordinates by adding
/// `row_offset` / `col_offset`. Column offset only applies on row 0
/// because subsequent rows reset to column 0 in the inner buffer.
#[inline]
pub fn offset_span(span: Span, row_offset: u32, col_offset: u32) -> Span {
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

/// Parse an embedded script / frontmatter block. Spans in the returned
/// `RawNode`s are already remapped to outer-file coordinates via
/// `row_offset` + `col_offset`. Returns empty vecs on grammar load failure
/// or parse budget exhaustion (matches the existing behaviour of each
/// per-language SFC parser).
pub fn parse_embedded_script(
    script_source: &[u8],
    query: &Query,
    capture_kind_by_idx: &[Option<NodeKind>],
    root_span_mask: u64,
    language: &Language,
    row_offset: u32,
    col_offset: u32,
) -> (Vec<RawNode>, Vec<RawImport>) {
    let mut parser = Parser::new();
    if parser.set_language(language).is_err() {
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
            } else if ci < 64 && (root_span_mask >> ci) & 1 != 0 {
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
                        field_reads: Vec::new(),
                        owner_class: None,
                        content_hash: 0,
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
