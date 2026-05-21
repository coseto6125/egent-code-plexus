//! C++ FunctionMeta extraction.
//!
//! All C rules plus:
//! - `is_async`: C++20 coroutines — heuristic: body contains `co_await` / `co_return` / `co_yield`.
//!   NOTE: there is no function-level keyword for coroutines; this is body-scan-based.
//! - `is_static`: `static` modifier on class method OR storage-class `static` on free function.
//! - `is_abstract`: pure-virtual declaration `virtual ... = 0;` (no body; ends with `= 0`).
//! - `is_generator`: body contains `co_yield` (C++20 coroutine generator pattern).
//! - `is_extern`: `extern "C"` linkage specifier OR declaration without body.
//! - visibility (class methods): track `public:` / `protected:` / `private:` section labels.
//!   Struct default = public (0); class default = private (2).
//! - params: same as C (`Type name` or unnamed).
//! - return_type: same as C; trailing return type `auto foo() -> Bar` → capture `Bar`.
//! - decorators: C++11 attributes `[[nodiscard]]` / `[[noreturn]]` / `[[deprecated]]` (strip `[[` / `]]`)
//!   + GCC/clang `__attribute__((...))` like C.

use super::{extract_with, node_text, subtree_contains_kind, ts_span};
use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta};
use tree_sitter::Node;

const CPP_FN_KINDS: &[&str] = &[
    "function_definition",
    "declaration",          // prototype / forward-declaration
    "function_declaration", // some grammar versions
    "field_declaration",    // member function declaration inside class (incl. pure virtual)
    "template_function",    // template fn with body
];

pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
    extract_with(
        root,
        source,
        nodes,
        file_category,
        CPP_FN_KINDS,
        extract_one,
    )
}

fn extract_one(
    fn_node: &Node<'_>,
    source: &[u8],
    raw: &RawNode,
    file_category: FileCategory,
) -> Option<RawFunctionMeta> {
    let mut flags: u16 = 0;

    // field_declaration is a class member declaration (pure virtual etc.) — not extern.
    // Free-function `declaration` (prototype) without body → extern.
    let is_free_decl_only = fn_node.kind() == "declaration";
    let is_field_decl = fn_node.kind() == "field_declaration";

    let mut has_extern = is_free_decl_only;
    let mut has_static = false;
    let mut has_virtual = false;
    let mut is_pure_virtual = false;

    // Scan direct children for storage-class / modifier keywords.
    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                match child.kind() {
                    "storage_class_specifier" => {
                        let txt = node_text(&child, source);
                        match txt {
                            "extern" => has_extern = true,
                            "static" => has_static = true,
                            _ => {}
                        }
                    }
                    "virtual" => {
                        has_virtual = true;
                    }
                    "linkage_specification" => {
                        // `extern "C"` linkage block.
                        has_extern = true;
                    }
                    _ => {}
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // Pure virtual detection: `virtual ... = 0` — tree-sitter-cpp represents
    // the `= 0` as a `virtual_specifier` or literal `0` after `=`.
    // We check the source text of the fn_node for `= 0` at the end.
    // For field_declaration (class member decl), `virtual` appears in the type specifier text.
    {
        let txt = node_text(fn_node, source).trim();
        let has_virtual_kw = has_virtual || txt.contains("virtual");
        if has_virtual_kw
            && (txt.ends_with("= 0")
                || txt.ends_with("=0")
                || txt.ends_with("= 0;")
                || txt.ends_with("=0;"))
        {
            is_pure_virtual = true;
        }
    }

    // For field_declaration: also check for `virtual_specifier` nodes (C++ grammar variant).
    if is_field_decl && !is_pure_virtual {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                if c.node().kind() == "virtual_specifier" {
                    is_pure_virtual = true;
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    if has_extern {
        flags |= FunctionMeta::FLAG_EXTERN;
    }
    if has_static {
        flags |= FunctionMeta::FLAG_STATIC;
    }
    if is_pure_virtual {
        flags |= FunctionMeta::FLAG_ABSTRACT;
    }

    // C++20 coroutine heuristic: body contains `co_await` / `co_return` / `co_yield`.
    // NOTE: no function-level marker exists; this is body-scan-based.
    let mut is_coroutine = false;
    let mut is_generator = false;
    if let Some(body) = fn_node.child_by_field_name("body") {
        if subtree_contains_kind(body, "co_await_expression")
            || subtree_contains_kind(body, "co_return_statement")
        {
            is_coroutine = true;
        }
        if subtree_contains_kind(body, "co_yield_expression") {
            is_generator = true;
            is_coroutine = true;
        }
    }
    if is_coroutine {
        flags |= FunctionMeta::FLAG_ASYNC;
    }
    if is_generator {
        flags |= FunctionMeta::FLAG_GENERATOR;
    }

    // is_test: FileCategory::Test (Google Test / Catch2 / doctest — file-path-only).
    // Also detect common Google Test macro patterns via function name prefix `Test_` / `TEST`.
    if file_category == FileCategory::Test
        || raw.name.starts_with("TEST")
        || raw.name.starts_with("Test_")
    {
        flags |= FunctionMeta::FLAG_TEST;
    }

    // visibility: infer from enclosing class/struct section labels.
    // Default: struct → public (0), class → private (2).
    let vis_code = infer_cpp_visibility(fn_node, source);
    flags |= vis_code << 6;

    // Parameters.
    let params = extract_params(fn_node, source);

    // Return type: `type` field; for trailing return `auto foo() -> Bar` check for
    // `trailing_return_type` node.
    let return_type = extract_return_type(fn_node, source);

    // Decorators: C++11 `[[attr]]` + GCC `__attribute__((...))`.
    let decorators = collect_cpp_decorators(fn_node, source);

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Infer C++ method visibility from enclosing `field_declaration_list` access specifiers.
/// Returns the 3-bit vis code: 0=public, 1=protected, 2=private.
/// Default: `struct` → public (0), `class` → private (2).
fn infer_cpp_visibility(fn_node: &Node<'_>, source: &[u8]) -> u16 {
    // Walk up to find field_declaration_list parent.
    let mut cur = fn_node.parent();
    while let Some(p) = cur {
        if p.kind() == "field_declaration_list" {
            // Determine default: parent of field_declaration_list is class_specifier or struct_specifier.
            let default_vis: u16 = p
                .parent()
                .map(|gp| {
                    if gp.kind() == "struct_specifier" {
                        0
                    } else {
                        2
                    }
                })
                .unwrap_or(2);

            // Scan siblings of fn_node within field_declaration_list for the closest
            // access_specifier preceding it.
            let mut last_vis = default_vis;
            let mut c = p.walk();
            if c.goto_first_child() {
                loop {
                    let sib = c.node();
                    if sib.id() == fn_node.id() {
                        break;
                    }
                    if sib.kind() == "access_specifier" {
                        let txt = node_text(&sib, source);
                        last_vis = match txt.trim().trim_end_matches(':') {
                            "public" => 0,
                            "protected" => 1,
                            "private" => 2,
                            _ => last_vis,
                        };
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
            }
            return last_vis;
        }
        cur = p.parent();
    }
    0 // Free functions → public
}

/// Extract the return type from a C++ function node.
/// Handles trailing return type `auto foo() -> Bar`.
fn extract_return_type(fn_node: &Node<'_>, source: &[u8]) -> String {
    // Check for trailing_return_type first.
    if let Some(decl) = find_function_declarator(*fn_node) {
        if let Some(trt) = decl.child_by_field_name("return_type") {
            // trailing_return_type includes `->` prefix; strip it.
            let txt = node_text(&trt, source);
            let stripped = txt.trim_start_matches("->").trim().to_string();
            if !stripped.is_empty() {
                return stripped;
            }
        }
    }
    // Standard: `type` field.
    fn_node
        .child_by_field_name("type")
        .map(|t| node_text(&t, source).to_string())
        .unwrap_or_default()
}

/// Collect C++11 `[[attr]]` and GCC `__attribute__((...))` decorators for a function.
fn collect_cpp_decorators(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();

    // 1. Direct child `attribute_declaration` nodes (C++11 [[attr]]).
    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                if child.kind() == "attribute_declaration" {
                    let txt = node_text(&child, source);
                    // Strip `[[` and `]]` and extract attribute names.
                    for attr in parse_cpp_attributes(txt) {
                        if !result.contains(&attr) {
                            result.push(attr);
                        }
                    }
                } else if child.kind() == "attribute_specifier"
                    || child.kind() == "ms_declspec_modifier"
                {
                    let txt = node_text(&child, source).to_string();
                    if !txt.is_empty() && !result.contains(&txt) {
                        result.push(txt);
                    }
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // 2. Preceding sibling `attribute_declaration` / `attribute_specifier` nodes.
    if let Some(parent) = fn_node.parent() {
        let mut pending: Vec<String> = Vec::new();
        let mut c = parent.walk();
        if c.goto_first_child() {
            loop {
                let sib = c.node();
                if sib.id() == fn_node.id() {
                    for attr in pending {
                        if !result.contains(&attr) {
                            result.push(attr);
                        }
                    }
                    break;
                }
                match sib.kind() {
                    "attribute_declaration" => {
                        let txt = node_text(&sib, source);
                        for attr in parse_cpp_attributes(txt) {
                            pending.push(attr);
                        }
                    }
                    "attribute_specifier" | "ms_declspec_modifier" => {
                        pending.push(node_text(&sib, source).to_string());
                    }
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

    result
}

/// Parse `[[nodiscard]]`, `[[nodiscard("reason")]]`, `[[deprecated, noreturn]]` etc.
/// Returns individual attribute names with `[[` / `]]` stripped.
fn parse_cpp_attributes(txt: &str) -> Vec<String> {
    let inner = txt
        .trim()
        .trim_start_matches("[[")
        .trim_end_matches("]]")
        .trim();
    inner
        .split(',')
        .map(|s| {
            // Strip arguments `(...)`.
            let base = s.trim().split('(').next().unwrap_or(s.trim()).trim();
            base.to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract flat `[name1, type1, name2, type2, ...]` from C++ function parameters.
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
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
            if child.kind() == "parameter_declaration"
                || child.kind() == "optional_parameter_declaration"
            {
                let name = child
                    .child_by_field_name("declarator")
                    .map(|d| extract_declarator_name(d, source))
                    .unwrap_or_default();
                let ty = child
                    .child_by_field_name("type")
                    .map(|t| node_text(&t, source).to_string())
                    .unwrap_or_default();
                // Skip void-only param lists.
                if ty == "void" && name.is_empty() {
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                    continue;
                }
                result.push(name);
                result.push(ty);
            } else if child.kind() == "variadic_parameter_declaration"
                || child.kind() == "variadic_parameter"
            {
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
            // Recurse one level for pointer/reference_declarator wrapping.
            if matches!(
                child.kind(),
                "pointer_declarator" | "reference_declarator" | "qualified_identifier"
            ) {
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

/// Unwrap nested pointer_declarator / array_declarator to get the innermost identifier text.
fn extract_declarator_name(decl: Node<'_>, source: &[u8]) -> String {
    match decl.kind() {
        "identifier" | "field_identifier" | "type_identifier" => {
            node_text(&decl, source).to_string()
        }
        "pointer_declarator"
        | "array_declarator"
        | "reference_declarator"
        | "abstract_pointer_declarator"
        | "abstract_reference_declarator" => {
            let mut cursor = decl.walk();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if !matches!(
                        child.kind(),
                        "*" | "&" | "&&" | "[" | "]" | "const" | "volatile"
                    ) {
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
