//! Python FunctionMeta extraction.
//!
//! Walks the tree-sitter AST to fill per-function flags/params/return_type.
//! Decorators are re-used from `RawNode.decorators` (already captured by the
//! Python parser in PR #244). A secondary tree walk is needed for async, is_generator,
//! and parameter/return_type detail that the query-based main pass doesn't capture.

use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta, NodeKind};
use tree_sitter::Node;

/// Span-keyed index entry: `(span, &RawNode)`.
type FnSpan<'a> = ((u32, u32, u32, u32), &'a RawNode);

/// Extract `RawFunctionMeta` for every Function/Method/Constructor node in
/// `nodes`. Caller passes the tree root and source so we can locate the
/// matching tree-sitter node by span.
pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
    // Build span→RawNode index for quick lookup during tree walk.
    let fn_spans: Vec<_> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            )
        })
        .map(|n| (n.span, n))
        .collect();

    if fn_spans.is_empty() {
        return vec![];
    }

    let mut out: Vec<RawFunctionMeta> = Vec::with_capacity(fn_spans.len());
    collect_function_nodes(root, source, &fn_spans, file_category, &mut out);
    out
}

/// Recursive tree walk — finds `function_definition` / `async_function_definition`
/// nodes and pairs them with `RawNode` entries by span.
fn collect_function_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let kind = node.kind();
    if kind == "function_definition" {
        let span = ts_node_span(&node);
        if let Some((_, raw)) = fn_spans.iter().find(|(s, _)| *s == span) {
            if let Some(meta) = extract_one(&node, source, raw, file_category) {
                out.push(meta);
            }
        }
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            collect_function_nodes(cursor.node(), source, fn_spans, file_category, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn ts_node_span(n: &Node<'_>) -> (u32, u32, u32, u32) {
    let s = n.start_position();
    let e = n.end_position();
    (s.row as u32, s.column as u32, e.row as u32, e.column as u32)
}

fn node_text<'a>(n: &Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[n.start_byte()..n.end_byte()]).unwrap_or("")
}

fn extract_one(
    fn_node: &Node<'_>,
    source: &[u8],
    raw: &RawNode,
    file_category: FileCategory,
) -> Option<RawFunctionMeta> {
    let mut flags: u16 = 0;

    // is_async: `async def` — the function_definition node starts with the `async`
    // keyword (CHOICE["async"|BLANK] as first grammar member). Check via source text
    // of the node's leading bytes (most reliable across tree-sitter-python versions).
    {
        let start = fn_node.start_byte();
        if source.get(start..start + 5).map(|s| s == b"async") == Some(true) {
            flags |= FunctionMeta::FLAG_ASYNC;
        }
    }

    // is_generator: walk the body block for `yield` or `yield from` expressions.
    if let Some(body) = fn_node.child_by_field_name("body") {
        if subtree_contains_yield(body) {
            flags |= FunctionMeta::FLAG_GENERATOR;
        }
    }

    // Decorators: walk from `fn_node.parent()` to find a `decorated_definition`
    // wrapper. In Python's AST, `@foo\ndef bar():` produces:
    //   decorated_definition
    //     decorator: "@foo"
    //     function_definition: "bar"
    // The decorator text node text includes the leading `@`; strip it.
    // Also merge any decorators already captured in `raw.decorators` (framework
    // queries capture route/signal decorators that may not be siblings here).
    let ast_decorators: Vec<String> = {
        let mut decs: Vec<String> = Vec::new();
        if let Some(parent) = fn_node.parent() {
            if parent.kind() == "decorated_definition" {
                let mut cur = parent.walk();
                if cur.goto_first_child() {
                    loop {
                        let child = cur.node();
                        if child.kind() == "decorator" {
                            // The decorator node text includes `@`; strip it.
                            let txt = node_text(&child, source)
                                .trim_start_matches('@')
                                .trim()
                                .to_string();
                            if !txt.is_empty() {
                                decs.push(txt);
                            }
                        }
                        if !cur.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
        }
        // Merge framework-captured decorators (raw.decorators) not already present.
        for d in &raw.decorators {
            let stripped = d.trim_start_matches('@').to_string();
            if !decs.contains(&stripped) {
                decs.push(stripped);
            }
        }
        decs
    };

    let mut is_static = false;
    let mut is_abstract = false;
    let mut is_test = false;
    for dec in &ast_decorators {
        match dec.as_str() {
            "staticmethod" => is_static = true,
            "abstractmethod" | "abc.abstractmethod" => is_abstract = true,
            _ => {}
        }
        // Test detection: pytest.fixture / pytest.mark.* / pytest.mark.parametrize etc.
        if dec.starts_with("pytest.") {
            is_test = true;
        }
    }

    if is_static {
        flags |= FunctionMeta::FLAG_STATIC;
    }
    if is_abstract {
        flags |= FunctionMeta::FLAG_ABSTRACT;
    }

    // is_test: file category OR name starts with `test_` OR pytest decorator found above.
    if is_test
        || file_category == FileCategory::Test
        || raw.name.starts_with("test_")
        || raw.name == "test"
    {
        flags |= FunctionMeta::FLAG_TEST;
    }

    // Visibility from name convention:
    // `__dunder__` → public (0), `__mangled` → private (2), `_private` → private (2), else → public (0).
    let vis: u16 = if raw.name.starts_with("__") && raw.name.ends_with("__") {
        0 // dunder → public
    } else if raw.name.starts_with('_') {
        2 // leading underscore → private
    } else {
        0 // default public
    };
    flags |= vis << 6;

    // Parameters.
    let params = extract_params(fn_node, source);

    // Return type.
    let return_type = fn_node
        .child_by_field_name("return_type")
        .map(|rt| {
            // The return_type field in Python is the node after `->`;
            // its text starts with `-> ` — strip that prefix if present.
            let txt = node_text(&rt, source);
            // tree-sitter-python includes the `->` in the return_type field text.
            txt.trim_start_matches("->").trim().to_string()
        })
        .unwrap_or_default();

    // Decorators collected from the AST walk above (source order, `@` stripped).
    let decorators = ast_decorators;

    Some(RawFunctionMeta {
        span: ts_node_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Walk a node subtree looking for `yield` or `yield from` expression nodes.
fn subtree_contains_yield(node: Node<'_>) -> bool {
    if matches!(node.kind(), "yield" | "yield_from") {
        return true;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            if subtree_contains_yield(cursor.node()) {
                return true;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    false
}

/// Extract `(name, type)` pairs from a `parameters` node.
/// Returns flat alternating `[name1, type1, name2, type2, ...]`.
/// Self/cls parameters are included as callers may want them.
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    let Some(params_node) = fn_node.child_by_field_name("parameters") else {
        return vec![];
    };
    let mut result = Vec::new();
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            match child.kind() {
                "identifier" => {
                    // Positional parameter with no type annotation.
                    result.push(node_text(&child, source).to_string());
                    result.push(String::new());
                }
                "typed_parameter" => {
                    // Grammar: `(identifier | list_splat_pattern | dict_splat_pattern) ":" (type: expr)`.
                    // The name has no named field; it's the first child. The type IS a named field.
                    let name = child
                        .child(0)
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    let ty = child
                        .child_by_field_name("type")
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        result.push(name);
                        result.push(ty);
                    }
                }
                "default_parameter" => {
                    // `name = default` — no type annotation.
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        result.push(name);
                        result.push(String::new());
                    }
                }
                "typed_default_parameter" => {
                    // `name: type = default`.
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    let ty = child
                        .child_by_field_name("type")
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        result.push(name);
                        result.push(ty);
                    }
                }
                "list_splat_pattern" | "dictionary_splat_pattern" => {
                    // `*args` or `**kwargs` — extract the inner identifier.
                    if let Some(inner) = child.child(1) {
                        result.push(node_text(&inner, source).to_string());
                        result.push(String::new());
                    }
                }
                _ => {}
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    result
}
