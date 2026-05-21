//! C FunctionMeta extraction.
//!
//! C-specific rules:
//! - `is_async`: always false.
//! - `is_static`: `static` storage class specifier on the function definition.
//! - `is_abstract`: always false (C has no abstract concept).
//! - `is_generator`: always false.
//! - `is_extern`: `extern` storage class OR function declaration without body
//!   (`declaration` at translation-unit level, not a `function_definition`).
//! - `is_test`: file in `tests/` directory (FileCategory::Test). C test frameworks
//!   (Check / Unity / CMocka) vary too much for reliable name-based detection.
//!   CAVEAT: this is file-path-only; framework-convention function names are not detected.
//! - visibility: C has no source-level visibility modifiers; all global fns are 0 (public).
//!   `static` linkage is captured via `is_static`. Always 0.
//! - params: `Type name` or just `Type` (unnamed). Capture name and type.
//! - return_type: type before function name declarator.
//! - decorators: GCC/clang `__attribute__((...))` and `#pragma` directives immediately
//!   preceding the function (best-effort heuristic from raw source scan).

use super::{extract_with, node_text, ts_span};
use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta};
use tree_sitter::Node;

/// Both function_definition (with body) and declaration (prototype) map to Function nodes.
const C_FN_KINDS: &[&str] = &["function_definition", "declaration"];

pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
    extract_with(root, source, nodes, file_category, C_FN_KINDS, extract_one)
}

fn extract_one(
    fn_node: &Node<'_>,
    source: &[u8],
    _raw: &RawNode,
    file_category: FileCategory,
) -> Option<RawFunctionMeta> {
    let mut flags: u16 = 0;

    // is_extern: declaration node (prototype) OR `extern` storage class specifier.
    let is_decl_only = fn_node.kind() == "declaration";
    let mut has_extern_specifier = false;
    let mut has_static_specifier = false;
    {
        let mut cursor = fn_node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "storage_class_specifier" {
                    let txt = node_text(&child, source);
                    match txt {
                        "extern" => has_extern_specifier = true,
                        "static" => has_static_specifier = true,
                        _ => {}
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    if is_decl_only || has_extern_specifier {
        flags |= FunctionMeta::FLAG_EXTERN;
    }
    if has_static_specifier {
        flags |= FunctionMeta::FLAG_STATIC;
    }

    // is_test: file category only (framework-specific names vary too much).
    if file_category == FileCategory::Test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    // visibility: always 0 (public) for C.
    // (is_static covers translation-unit-local linkage separately above.)

    // Parameters.
    let params = extract_params(fn_node, source);

    // Return type: the `type` field of function_definition / declaration.
    let return_type = fn_node
        .child_by_field_name("type")
        .map(|t| node_text(&t, source).to_string())
        .unwrap_or_default();

    // Decorators: scan preceding siblings for `__attribute__((...))` and attribute_specifier nodes.
    // Also scan raw source for `#pragma` lines immediately above the function start line.
    let decorators = collect_c_decorators(fn_node, source);

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Collect GCC/clang attribute decorators and `#pragma` directives for a C function.
/// Checks:
/// 1. Sibling `attribute_specifier` / `__attribute__` nodes preceding this function.
/// 2. `#pragma` lines in source immediately above the function start row.
fn collect_c_decorators(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();

    // 1. Check for `__attribute__((...))` in sibling nodes immediately preceding fn.
    if let Some(parent) = fn_node.parent() {
        let mut pending: Vec<String> = Vec::new();
        let mut c = parent.walk();
        if c.goto_first_child() {
            loop {
                let sib = c.node();
                if sib.id() == fn_node.id() {
                    result.append(&mut pending);
                    break;
                }
                match sib.kind() {
                    "attribute_specifier" | "ms_declspec_modifier" => {
                        let txt = node_text(&sib, source).to_string();
                        if !txt.is_empty() {
                            pending.push(txt);
                        }
                    }
                    // Non-attribute node resets accumulator.
                    _ => {
                        pending.clear();
                    }
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // 2. Scan `__attribute__((...))` text also inside function type children.
    {
        let mut cursor = fn_node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "attribute_specifier" || child.kind() == "ms_declspec_modifier" {
                    let txt = node_text(&child, source).to_string();
                    if !txt.is_empty() && !result.contains(&txt) {
                        result.push(txt);
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // 3. `#pragma` lines immediately above function start row.
    let fn_start_row = fn_node.start_position().row;
    if fn_start_row > 0 {
        let text = std::str::from_utf8(source).unwrap_or("");
        let lines: Vec<&str> = text.lines().collect();
        let mut row = fn_start_row;
        let mut pragma_buf: Vec<String> = Vec::new();
        loop {
            if row == 0 {
                break;
            }
            row -= 1;
            let line = lines.get(row).copied().unwrap_or("").trim();
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix("#pragma ") {
                pragma_buf.push(format!("#pragma {rest}"));
            } else {
                break;
            }
        }
        pragma_buf.reverse();
        result.extend(pragma_buf);
    }

    result
}

/// Extract flat `[name1, type1, name2, type2, ...]` from C function parameters.
/// Handles named and unnamed parameters (`void` param list → empty).
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    // Find the function_declarator inside the fn_node to get to parameters.
    let Some(decl_node) = find_function_declarator(*fn_node) else {
        return vec![];
    };
    let Some(params_node) = decl_node.child_by_field_name("parameters") else {
        return vec![];
    };
    let mut result = Vec::new();
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == "parameter_declaration" {
                // Type: the `type` field (or entire source span minus the name).
                // Name: `declarator` field — may be absent for unnamed params.
                let name = child
                    .child_by_field_name("declarator")
                    .map(|d| {
                        // Declarator may be a pointer_declarator wrapping an identifier.
                        extract_declarator_name(d, source)
                    })
                    .unwrap_or_default();
                let ty = child
                    .child_by_field_name("type")
                    .map(|t| node_text(&t, source).to_string())
                    .unwrap_or_default();
                // Skip `void` single-param declarations (`int foo(void)`).
                if ty == "void" && name.is_empty() {
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                    continue;
                }
                result.push(name);
                result.push(ty);
            } else if child.kind() == "variadic_parameter" {
                result.push("...".to_string());
                result.push(String::new());
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    result
}

/// Walk a function definition / declaration node to find the inner `function_declarator`.
fn find_function_declarator(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == "function_declarator" {
                return Some(child);
            }
            // Recurse one level for pointer_declarator wrapping function_declarator.
            if child.kind() == "pointer_declarator" {
                if let Some(inner) = find_function_declarator(child) {
                    return Some(inner);
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

/// Unwrap nested pointer_declarator / array_declarator to get the innermost identifier.
fn extract_declarator_name(decl: Node<'_>, source: &[u8]) -> String {
    match decl.kind() {
        "identifier" => node_text(&decl, source).to_string(),
        "pointer_declarator" | "array_declarator" | "reference_declarator" => {
            // Recurse into the inner declarator child.
            let mut cursor = decl.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.kind() != "*" && child.kind() != "&" && child.kind() != "[" {
                        return extract_declarator_name(child, source);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            String::new()
        }
        _ => node_text(&decl, source).to_string(),
    }
}
