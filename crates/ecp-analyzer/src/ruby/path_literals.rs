//! Ruby-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` handles `string` nodes (single- and
//! double-quoted share the same kind). Interpolated strings
//! (`string_interpolation` child) are filtered internally. Invoked from
//! `receiver_types::extract_ruby_calls_and_path_literals` so a single
//! DFS handles both call attribution and path-literal collection.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    // Skip interpolated strings: any `string_interpolation` child means dynamic content.
    let mut c = str_node.walk();
    for child in str_node.children(&mut c) {
        if child.kind() == "string_interpolation" {
            return None;
        }
    }

    // Extract the string content from `string_content` child.
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

/// Extract the content of a `string` node. Looks for `string_content` child;
/// if absent, strips surrounding quotes from the raw node text.
fn extract_string_content<'a>(str_node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    // Try to find the string_content child node.
    let mut c = str_node.walk();
    for child in str_node.children(&mut c) {
        if child.kind() == "string_content" {
            let raw = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok()?;
            return Some(raw);
        }
    }
    // Fallback: strip quotes from the whole node text.
    let raw = std::str::from_utf8(&source[str_node.start_byte()..str_node.end_byte()]).ok()?;
    strip_quotes(raw)
}

/// Strip surrounding quotes from a Ruby string literal.
/// Handles `'...'` and `"..."`. Returns `None` if shape is malformed.
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

/// Climb the AST from a string literal to find the enclosing `call` expression
/// and resolve its method name. Returns `None` when the literal is not inside a call.
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    // Ruby AST: string → argument_list → call
    let parent = str_node.parent()?;
    let call_node = if parent.kind() == "argument_list" {
        parent.parent()?
    } else {
        return None;
    };
    if call_node.kind() != "call" {
        return None;
    }
    let method_node = call_node.child_by_field_name("method")?;
    std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()])
        .ok()
        .map(str::to_string)
}

/// Climb the AST from a string literal to find the enclosing `method` or
/// `singleton_method`, and optionally the enclosing `class` or `module`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "method" | "singleton_method" if function_name.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "class" | "module" if owner.is_none() => {
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
