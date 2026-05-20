//! TypeScript FunctionMeta extraction.
//!
//! Handles `async`, `static`, `abstract` modifiers, generator (`function*` /
//! `*method`), TypeScript access modifiers (`public`/`protected`/`private`),
//! and `declare function` ambient declarations.

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

/// Relevant tree-sitter node kinds for TypeScript functions.
const TS_FN_KINDS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration", // `function* gen()`
    "function",
    "generator_function", // expression form `function* ()`
    "method_definition",
    "arrow_function",
    "ambient_declaration",       // `declare function ...`
    "function_signature",        // interface method signatures
    "method_signature",          // interface method signatures
    "abstract_method_signature", // `abstract compute(): T` in abstract class
    "constructor_type",
];

fn collect_fn_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let k = node.kind();
    if TS_FN_KINDS.contains(&k) {
        let span = ts_span(&node);
        if let Some((_, raw)) = fn_spans.iter().find(|(s, _)| *s == span) {
            if let Some(meta) = extract_one(&node, source, raw, file_category) {
                out.push(meta);
            }
            // Don't recurse into the matched function to avoid double-counting nested fns.
            return;
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

    // Scan direct children for keyword modifiers.
    let mut vis_code: u16 = 0; // default public
    let mut is_extern = false;

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
                    "abstract" => {
                        flags |= FunctionMeta::FLAG_ABSTRACT;
                    }
                    "*" => {
                        // generator method or function*
                        flags |= FunctionMeta::FLAG_GENERATOR;
                    }
                    "accessibility_modifier" => {
                        let txt = node_text(&child, source);
                        vis_code = ts_access_modifier(txt);
                    }
                    "declare" => {
                        is_extern = true;
                    }
                    _ => {}
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // generator_function_declaration / generator_function node kinds are generators by definition.
    if fn_node.kind() == "generator_function_declaration" || fn_node.kind() == "generator_function"
    {
        flags |= FunctionMeta::FLAG_GENERATOR;
    }
    // abstract_method_signature is abstract by definition.
    if fn_node.kind() == "abstract_method_signature" {
        flags |= FunctionMeta::FLAG_ABSTRACT;
    }

    if is_extern {
        flags |= FunctionMeta::FLAG_EXTERN;
    }

    // is_test: file category, or function name matches common test framework patterns.
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
    let is_test = file_category == FileCategory::Test
        || test_names.contains(&&*raw.name)
        || raw.name.starts_with("test")
            && raw
                .name
                .chars()
                .nth(4)
                .map(|c| c.is_uppercase() || c == '_')
                .unwrap_or(true);
    if is_test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    flags |= vis_code << 6;

    // Decorators from RawNode (already captured by the TS parser).
    let decorators: Vec<String> = raw
        .decorators
        .iter()
        .map(|d| d.trim_start_matches('@').to_string())
        .collect();

    let params = extract_params(fn_node, source);

    // Return type: `type_annotation` child (`: ReturnType`).
    let return_type = find_return_type(fn_node, source);

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Convert a TypeScript accessibility modifier text to a 3-bit vis code.
fn ts_access_modifier(txt: &str) -> u16 {
    match txt.trim() {
        "public" => 0,
        "protected" => 1,
        "private" => 2,
        _ => 0,
    }
}

/// Extract `[name1, type1, name2, type2, ...]` from TS/JS formal_parameters.
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
                "required_parameter" | "optional_parameter" => {
                    // `name: Type` or `name?: Type` — field names: `pattern` and `type`.
                    let name = child
                        .child_by_field_name("pattern")
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    let ty = child
                        .child_by_field_name("type")
                        .map(|n| {
                            // type_annotation node includes the `:`, strip it.
                            let txt = node_text(&n, source);
                            txt.trim_start_matches(':').trim().to_string()
                        })
                        .unwrap_or_default();
                    if !name.is_empty() {
                        result.push(name);
                        result.push(ty);
                    }
                }
                "assignment_pattern" => {
                    // `name = default` (no type).
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
                    // `...args` — no type unless typed.
                    let txt = node_text(&child, source);
                    result.push(txt.to_string());
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

/// Find the return type annotation for a function node.
/// Looks for `type_annotation` child after `)`.
fn find_return_type(fn_node: &Node<'_>, source: &[u8]) -> String {
    fn_node
        .child_by_field_name("return_type")
        .map(|rt| {
            let txt = node_text(&rt, source);
            // type_annotation includes `:` — strip it.
            txt.trim_start_matches(':').trim().to_string()
        })
        .unwrap_or_default()
}
