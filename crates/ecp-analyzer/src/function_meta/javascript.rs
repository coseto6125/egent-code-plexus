//! JavaScript FunctionMeta extraction.
//!
//! JS has no type annotations. Detects: `async`, `static`, `function*`,
//! `#private` fields (ES2022 private visibility), test function name patterns,
//! and legacy/stage-3 decorators. `is_abstract` and `is_extern` are always false.

use super::{extract_with, node_text, ts_span};
use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta};
use tree_sitter::Node;

const JS_FN_KINDS: &[&str] = &[
    "function_declaration",
    "function",
    "function_expression",
    "method_definition",
    "arrow_function",
    "generator_function_declaration",
    "generator_function",
];

pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
    extract_with(root, source, nodes, file_category, JS_FN_KINDS, extract_one)
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
