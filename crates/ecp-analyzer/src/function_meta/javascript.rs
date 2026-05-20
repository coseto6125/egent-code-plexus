//! JavaScript FunctionMeta extraction.
//!
//! JS has no type annotations. Detects: `async`, `static`, `function*`,
//! `#private` fields (ES2022 private visibility), test function name patterns,
//! and legacy/stage-3 decorators. `is_abstract` and `is_extern` are always false.

use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta, NodeKind};
use tree_sitter::Node;

/// Span-keyed index entry: `(span, &RawNode)`.
type FnSpan<'a> = ((u32, u32, u32, u32), &'a RawNode);

pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
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
    collect_fn_nodes(root, source, &fn_spans, file_category, &mut out);
    out
}

fn ts_span(n: &Node<'_>) -> (u32, u32, u32, u32) {
    let s = n.start_position();
    let e = n.end_position();
    (s.row as u32, s.column as u32, e.row as u32, e.column as u32)
}

fn node_text<'a>(n: &Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[n.start_byte()..n.end_byte()]).unwrap_or("")
}

const JS_FN_KINDS: &[&str] = &[
    "function_declaration",
    "function",
    "function_expression",
    "method_definition",
    "arrow_function",
    "generator_function_declaration",
    "generator_function",
];

fn collect_fn_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let k = node.kind();
    if JS_FN_KINDS.contains(&k) {
        let span = ts_span(&node);
        if let Some((_, raw)) = fn_spans.iter().find(|(s, _)| *s == span) {
            if let Some(meta) = extract_one(&node, source, raw, file_category) {
                out.push(meta);
                return; // don't recurse into this function
            }
        }
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            collect_fn_nodes(cursor.node(), source, fn_spans, file_category, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn extract_one(
    fn_node: &Node<'_>,
    source: &[u8],
    raw: &RawNode,
    file_category: FileCategory,
) -> Option<RawFunctionMeta> {
    let mut flags: u16 = 0;

    // generator_function_declaration / generator_function node kinds are already generators.
    let kind = fn_node.kind();
    if kind == "generator_function_declaration" || kind == "generator_function" {
        flags |= FunctionMeta::FLAG_GENERATOR;
    }

    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                match child.kind() {
                    "async" => {
                        flags |= FunctionMeta::FLAG_ASYNC;
                    }
                    "static" => {
                        flags |= FunctionMeta::FLAG_STATIC;
                    }
                    "*" => {
                        // method_definition with generator: `*foo() {}`
                        flags |= FunctionMeta::FLAG_GENERATOR;
                    }
                    _ => {}
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // Visibility: ES2022 `#name` private class fields/methods.
    // In tree-sitter-javascript, a method_definition's name is a `property_identifier`
    // or `private_property_identifier` (starts with `#`).
    let vis_code: u16 = if raw.name.starts_with('#') { 2 } else { 0 };
    flags |= vis_code << 6;

    // is_test: file category or common test runner function names.
    let test_names = &[
        "test",
        "it",
        "describe",
        "beforeAll",
        "afterAll",
        "beforeEach",
        "afterEach",
        "xit",
        "xdescribe",
        "fit",
        "fdescribe",
    ];
    let is_test = file_category == FileCategory::Test || test_names.contains(&&*raw.name);
    if is_test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    // Decorators from RawNode.
    let decorators: Vec<String> = raw
        .decorators
        .iter()
        .map(|d| d.trim_start_matches('@').to_string())
        .collect();

    let params = extract_params(fn_node, source);

    // JS has no return type annotations.
    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type: String::new(),
        decorators,
    })
}

/// Extract `[name1, "", name2, "", ...]` — JS has no types so type entries are empty.
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
                    result.push(node_text(&child, source).to_string());
                    result.push(String::new());
                }
                "assignment_pattern" => {
                    // `name = default`.
                    let name = child
                        .child_by_field_name("left")
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        result.push(name);
                        result.push(String::new());
                    }
                }
                "rest_pattern" => {
                    result.push(node_text(&child, source).to_string());
                    result.push(String::new());
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
