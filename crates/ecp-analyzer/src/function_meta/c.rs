//! C FunctionMeta extraction.
//!
//! C-specific rules:
//! - `is_async`: always false.
//! - `is_static`: `static` storage class specifier on the function definition.
//! - `is_abstract`: always false (C has no abstract concept).
//! - `is_generator`: always false.
//! - `is_extern`: function declaration without body (`declaration` at
//!   translation-unit level, not a `function_definition`). The `extern`
//!   keyword on its own sets `FLAG_C_EXTERN_STORAGE`.
//! - parameter form (bits 11-12): `f(int)` / `f(void)` prototype list, `f()`
//!   empty parens, `f(a, b) int a;` K&R identifier list.
//! - `is_test`: file in `tests/` directory (FileCategory::Test). C test frameworks
//!   (Check / Unity / CMocka) vary too much for reliable name-based detection.
//!   CAVEAT: this is file-path-only; framework-convention function names are not detected.
//! - visibility: C has no source-level visibility modifiers; all global fns are 0 (public).
//!   `static` linkage is captured via `is_static`. Always 0.
//! - params: `Type name` or just `Type` (unnamed). Capture name and type.
//! - return_type: type before function name declarator.
//! - decorators: GCC/clang `__attribute__((...))` and `#pragma` directives immediately
//!   preceding the function (best-effort heuristic from raw source scan).

use super::{c_param_type, extract_with, node_text, ts_span};
use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta};
use tree_sitter::Node;

/// C89 gives a K&R parameter that no declaration types the type `int`.
const KR_IMPLICIT_INT: bool = true;

/// Both function_definition (with body) and declaration (prototype) map to Function nodes.
const C_FN_KINDS: &[&str] = &["function_definition", "declaration"];

pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
    // Hoist `text.lines().collect()` once per file (previously per function
    // in the #pragma scan below). Same pattern as Go — see
    // `function_meta::go::extract` for rationale.
    let text = std::str::from_utf8(source).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    let lines_ref = &lines;
    extract_with(
        root,
        source,
        nodes,
        file_category,
        C_FN_KINDS,
        |fn_node, src, raw, fc| extract_one(fn_node, src, lines_ref, raw, fc),
    )
}

fn extract_one(
    fn_node: &Node<'_>,
    source: &[u8],
    lines: &[&str],
    _raw: &RawNode,
    file_category: FileCategory,
) -> Option<RawFunctionMeta> {
    let mut flags: u16 = 0;

    // is_extern: declaration node (prototype); `extern` is a storage class of its own.
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

    if is_decl_only {
        flags |= FunctionMeta::FLAG_EXTERN;
    }
    if has_extern_specifier {
        flags |= FunctionMeta::FLAG_C_EXTERN_STORAGE;
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
    let (params, param_form) = extract_params(fn_node, source);
    flags |= param_form;

    // Return type: the `type` field of function_definition / declaration.
    let return_type = fn_node
        .child_by_field_name("type")
        .map(|t| node_text(&t, source).to_string())
        .unwrap_or_default();

    // Decorators: scan preceding siblings for `__attribute__((...))` and attribute_specifier nodes.
    // Also scan raw source for `#pragma` lines immediately above the function start line.
    let decorators = collect_c_decorators(fn_node, source, lines);

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
        // No generic syntax in this language.
        type_params: Vec::new(),
    })
}

/// Collect GCC/clang attribute decorators and `#pragma` directives for a C function.
/// Checks:
/// 1. Sibling `attribute_specifier` / `__attribute__` nodes preceding this function.
/// 2. `#pragma` lines in source immediately above the function start row.
///
/// `lines` is hoisted by the file-level `extract` so the `#pragma` scan
/// reuses one split across all functions in the file.
fn collect_c_decorators(fn_node: &Node<'_>, source: &[u8], lines: &[&str]) -> Vec<String> {
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
    // `lines` hoisted by caller (see `extract` doc).
    let fn_start_row = fn_node.start_position().row;
    if fn_start_row > 0 {
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
/// Handles named and unnamed parameters (`void` param list → empty) and a
/// K&R identifier list. The second value is the parameter-form bits of
/// `FunctionMeta::flags`: `f()` and a K&R list differ from a prototype list.
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> (Vec<String>, u16) {
    // Find the function_declarator inside the fn_node to get to parameters.
    let Some(decl_node) = find_function_declarator(*fn_node) else {
        return (vec![], 0);
    };
    let Some(params_node) = decl_node.child_by_field_name("parameters") else {
        return (vec![], 0);
    };
    let mut result = Vec::new();
    let mut listed_any = false;
    let mut old_style = false;
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            listed_any |= matches!(
                child.kind(),
                "parameter_declaration" | "identifier" | "variadic_parameter"
            );
            if child.kind() == "parameter_declaration" {
                // Type: the `type` field (or entire source span minus the name).
                // Name: `declarator` field — may be absent for unnamed params.
                let declarator = child.child_by_field_name("declarator");
                let name = declarator
                    .map(|d| {
                        // Declarator may be a pointer_declarator wrapping an identifier.
                        extract_declarator_name(d, source)
                    })
                    .unwrap_or_default();
                let base = child
                    .child_by_field_name("type")
                    .map(|t| node_text(&t, source))
                    .unwrap_or_default();
                // Skip `void` single-param declarations (`int foo(void)`).
                if base == "void" && declarator.is_none() {
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                    continue;
                }
                result.push(name);
                result.push(c_param_type(base, declarator, source));
            } else if child.kind() == "identifier" {
                // A K&R list (`int kr(a, b) int a; int b; { .. }`) names its
                // parameters; their types come from the declarations before
                // the body, and are `int` (implicit, [`KR_IMPLICIT_INT`]) when none declares one.
                let name = node_text(&child, source);
                old_style = true;
                result.push(name.to_string());
                result.push(
                    old_style_param_type(fn_node, name, source)
                        .unwrap_or_else(|| if KR_IMPLICIT_INT { "int" } else { "" }.to_string()),
                );
            } else if child.kind() == "variadic_parameter" {
                result.push("...".to_string());
                result.push(String::new());
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    let form = if old_style {
        FunctionMeta::C_PARAM_FORM_OLD_STYLE
    } else if listed_any {
        0
    } else {
        FunctionMeta::C_PARAM_FORM_EMPTY_PARENS
    };
    (result, form)
}

/// The type that a K&R definition's parameter declarations (`int a;`
/// between the declarator and the body) give the parameter `name`.
fn old_style_param_type(fn_node: &Node<'_>, name: &str, source: &[u8]) -> Option<String> {
    let mut cursor = fn_node.walk();
    let found = fn_node
        .children(&mut cursor)
        .filter(|child| child.kind() == "declaration")
        .find_map(|declaration| {
            let mut declarators = declaration.walk();
            let declarator = declaration
                .children_by_field_name("declarator", &mut declarators)
                .find(|d| extract_declarator_name(*d, source) == name)?;
            declaration
                .child_by_field_name("type")
                .map(|ty| c_param_type(node_text(&ty, source), Some(declarator), source))
        });
    found
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
    // A pointer, array or function declarator names its inner declarator
    // in a field: `* const p`, `(*cb)(int)`.
    if let Some(inner) = decl.child_by_field_name("declarator") {
        return extract_declarator_name(inner, source);
    }
    match decl.kind() {
        "parenthesized_declarator" => decl
            .named_child(0)
            .map(|inner| extract_declarator_name(inner, source))
            .unwrap_or_default(),
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
