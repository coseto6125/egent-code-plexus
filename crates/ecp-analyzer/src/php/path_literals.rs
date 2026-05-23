//! PHP-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` handles `string` (single-quoted `'...'`) and
//! `encapsed_string` (double-quoted `"..."`, interpolated forms filtered
//! by `variable_name` child check). Invoked from
//! `receiver_types::extract_php_calls_and_path_literals` so a single
//! DFS handles both call attribution and path-literal collection.
//!
//! `heredoc_body` / `nowdoc_body` are not handled here (rare for path
//! literals, deferred to a later improvement task).

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let value = extract_string_content(str_node, source)?;
    if !is_path_shaped(value) {
        return None;
    }

    let callee = enclosing_callee(str_node, source);
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

/// Extract the string content, skipping interpolated forms.
/// Returns `None` for `encapsed_string` with variable interpolation.
fn extract_string_content<'a>(str_node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    match str_node.kind() {
        "string" => {
            // PHP single-quoted string: content is in `string_value` child.
            let mut c = str_node.walk();
            for child in str_node.children(&mut c) {
                if child.kind() == "string_value" {
                    return std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
                }
            }
            // Fallback: strip quotes from raw text.
            let raw =
                std::str::from_utf8(&source[str_node.start_byte()..str_node.end_byte()]).ok()?;
            strip_quotes(raw)
        }
        "encapsed_string" => {
            // PHP double-quoted string. Skip if it has variable_name children (interpolation).
            let mut c = str_node.walk();
            let mut content: Option<&'a str> = None;
            for child in str_node.children(&mut c) {
                match child.kind() {
                    "variable_name" | "encapsed_string_chars_after_variable" => {
                        // Has interpolation — skip the whole string.
                        return None;
                    }
                    "string_value" | "string_content" => {
                        content =
                            std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
                    }
                    _ => {}
                }
            }
            // Fallback: tree-sitter-php may not always expose `string_value` /
            // `string_content` as a child — strip quotes from the raw text.
            if content.is_none() {
                let raw = std::str::from_utf8(&source[str_node.start_byte()..str_node.end_byte()])
                    .ok()?;
                content = strip_quotes(raw);
            }
            content
        }
        _ => None,
    }
}

/// Strip surrounding single or double quotes from a PHP string literal raw text.
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let first = bytes[0];
    let last = *bytes.last()?;
    if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
        std::str::from_utf8(&bytes[1..bytes.len() - 1]).ok()
    } else {
        None
    }
}

/// Climb the AST from a string literal to find the enclosing function call
/// and resolve its callee name. PHP call shapes:
///   `function_call_expression` → `function` field
///   `member_call_expression` → `name` field
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    let call_node = if parent.kind() == "arguments" {
        parent.parent()?
    } else if parent.kind() == "argument" {
        parent.parent().and_then(|p| {
            if p.kind() == "arguments" {
                p.parent()
            } else {
                None
            }
        })?
    } else {
        return None;
    };

    match call_node.kind() {
        "function_call_expression" => {
            let fn_node = call_node.child_by_field_name("function")?;
            std::str::from_utf8(&source[fn_node.start_byte()..fn_node.end_byte()])
                .ok()
                .map(str::to_string)
        }
        "member_call_expression" => {
            let name_node = call_node.child_by_field_name("name")?;
            std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                .ok()
                .map(str::to_string)
        }
        "scoped_call_expression" => {
            let name_node = call_node.child_by_field_name("name")?;
            std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                .ok()
                .map(str::to_string)
        }
        _ => None,
    }
}

/// Climb the AST from a string literal to find the enclosing
/// `function_definition` or `method_declaration`, and optionally the
/// enclosing `class_declaration`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_definition" | "method_declaration" if function_name.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "class_declaration" if owner.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    owner =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            _ => {}
        }
        cur = n.parent();
    }
    (function_name, owner)
}
