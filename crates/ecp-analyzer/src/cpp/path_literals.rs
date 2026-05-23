//! C++-side helpers for `RawPathLiteral` extraction. Entry points
//! `build_raw_path_literal` (`string_literal` / `raw_string_literal`)
//! and `build_concatenated` (`concatenated_string`) are invoked from
//! `receiver_types::extract_cpp_calls_and_path_literals` so a single
//! DFS handles both call attribution and path-literal collection.
//!
//! C++ string forms handled:
//!   `"foo"`              — ordinary string literal
//!   `L"foo"` / `u8"foo"` / `u"foo"` / `U"foo"`  — encoding prefixes (plain)
//!   `R"delim(foo)delim"` — raw string literal (any delimiter)
//!   `"foo" "bar"`        — concatenated_string
//!
//! For method definitions whose declarator is `Foo::method`, the owner
//! `Foo` is captured into `enclosing_owner`. In-class method bodies (inside
//! `class_specifier` / `struct_specifier`) use the class name as owner.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_ext_change_callee, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let raw_bytes = &source[str_node.start_byte()..str_node.end_byte()];
    let raw = std::str::from_utf8(raw_bytes).ok()?;
    let value = strip_quotes(raw, str_node.kind() == "raw_string_literal")?;
    let callee = enclosing_callee(str_node, source);
    if !is_path_shaped(value) && !is_ext_change_callee(callee.as_deref()) {
        return None;
    }
    let (kind, conf) = classify_sink(callee.as_deref());
    let reason = sink_reason(kind, conf);

    let (enclosing_symbol, enclosing_owner) = enclosing_symbol_and_owner(str_node, source);

    let pos = str_node.start_position();
    let end = str_node.end_position();
    Some(RawPathLiteral {
        value: value.to_string(),
        span: (
            pos.row as u32,
            pos.column as u32,
            end.row as u32,
            end.column as u32,
        ),
        enclosing_symbol,
        enclosing_owner,
        sink_reason: reason,
    })
}

pub(super) fn build_concatenated(concat_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let mut joined = String::new();
    let mut cursor = concat_node.walk();
    for child in concat_node.children(&mut cursor) {
        if matches!(child.kind(), "string_literal" | "raw_string_literal") {
            let raw_bytes = &source[child.start_byte()..child.end_byte()];
            let raw = std::str::from_utf8(raw_bytes).ok()?;
            let piece = strip_quotes(raw, child.kind() == "raw_string_literal")?;
            joined.push_str(piece);
        }
    }
    if joined.is_empty() || !is_path_shaped(&joined) {
        return None;
    }

    let callee = enclosing_callee(concat_node, source);
    let (kind, conf) = classify_sink(callee.as_deref());
    let reason = sink_reason(kind, conf);
    let (enclosing_symbol, enclosing_owner) = enclosing_symbol_and_owner(concat_node, source);

    let pos = concat_node.start_position();
    let end = concat_node.end_position();
    Some(RawPathLiteral {
        value: joined,
        span: (
            pos.row as u32,
            pos.column as u32,
            end.row as u32,
            end.column as u32,
        ),
        enclosing_symbol,
        enclosing_owner,
        sink_reason: reason,
    })
}

/// Strip surrounding quotes from a C++ string literal capture.
/// `is_raw=true` parses `R"delim(body)delim"`; otherwise standard / prefixed.
fn strip_quotes(raw: &str, is_raw: bool) -> Option<&str> {
    let bytes = raw.as_bytes();
    if is_raw {
        let r_pos = bytes.iter().position(|&b| b == b'R')?;
        let after_r = r_pos + 1;
        if bytes.get(after_r) != Some(&b'"') {
            return None;
        }
        let body_start_paren = after_r + 1;
        let open_paren = bytes[body_start_paren..]
            .iter()
            .position(|&b| b == b'(')
            .map(|p| body_start_paren + p)?;
        let delim = &bytes[body_start_paren..open_paren];
        let close_pat_len = delim.len() + 2;
        let close_start = bytes.len().checked_sub(close_pat_len + 1)?;
        if bytes.last() != Some(&b'"') {
            return None;
        }
        if &bytes[close_start..close_start + 1] != b")" {
            return None;
        }
        std::str::from_utf8(&bytes[open_paren + 1..close_start]).ok()
    } else {
        let mut i = 0;
        if matches!(bytes.first(), Some(b'L' | b'u' | b'U')) {
            if bytes.get(i + 1) == Some(&b'8') {
                i += 2;
            } else {
                i += 1;
            }
        }
        if bytes.get(i) != Some(&b'"') {
            return None;
        }
        let body_start = i + 1;
        let body_end = bytes.len().checked_sub(1)?;
        if body_end < body_start || bytes.get(body_end) != Some(&b'"') {
            return None;
        }
        std::str::from_utf8(&bytes[body_start..body_end]).ok()
    }
}

fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    if parent.kind() != "argument_list" {
        return None;
    }
    let call = parent.parent()?;
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    callee_name(function, source)
}

fn callee_name(function: Node<'_>, source: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(&source[function.start_byte()..function.end_byte()]).ok()?;
    Some(text.to_string())
}

/// Climb the AST from a string literal to find the enclosing
/// `function_definition` plus its owner class (when nested inside a
/// `class_specifier` / `struct_specifier`, or when the declarator is
/// `qualified_identifier` like `Foo::method`).
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut owner_from_class: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_definition" => {
                let (fn_name, owner_from_qual) = fn_definition_name_and_owner(n, source);
                let owner = owner_from_qual.or(owner_from_class);
                return (fn_name, owner);
            }
            "class_specifier" | "struct_specifier" if owner_from_class.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    owner_from_class =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            _ => {}
        }
        cur = n.parent();
    }
    (None, owner_from_class)
}

/// Extract the function name and (for qualified declarators like
/// `Foo::method`) the owner class from a C++ `function_definition`.
fn fn_definition_name_and_owner(
    fn_def: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let Some(declarator) = fn_def.child_by_field_name("declarator") else {
        return (None, None);
    };
    drill_declarator(declarator, source)
}

fn drill_declarator(node: Node<'_>, source: &[u8]) -> (Option<String>, Option<String>) {
    match node.kind() {
        "identifier" | "field_identifier" => (
            std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                .ok()
                .map(str::to_string),
            None,
        ),
        "qualified_identifier" => {
            // scope: `Foo`, name: identifier/destructor/etc.
            let owner = node.child_by_field_name("scope").and_then(|s| {
                std::str::from_utf8(&source[s.start_byte()..s.end_byte()])
                    .ok()
                    .map(str::to_string)
            });
            let fn_name = node.child_by_field_name("name").and_then(|n| {
                std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                    .ok()
                    .map(str::to_string)
            });
            (fn_name, owner)
        }
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator" => match node.child_by_field_name("declarator") {
            Some(inner) => drill_declarator(inner, source),
            None => (None, None),
        },
        _ => (None, None),
    }
}
