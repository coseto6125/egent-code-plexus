//! Ruby-side extractor for `RawPathLiteral` entries. Walks the file's
//! `string` nodes, filters interpolated strings, filters via
//! `path_literal::is_path_shaped`, classifies the call-context sink via
//! `path_literal::classify_sink`, and resolves the enclosing method / class.
//!
//! Ruby string nodes: `(string (string_content))` for double-quoted, and
//! single-quoted `(string (string_content))` — tree-sitter-ruby uses the
//! same `string` parent node for both. Interpolated strings contain a
//! `string_interpolation` child — those are skipped entirely.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

/// Walk the Ruby tree-sitter `tree` and emit one `RawPathLiteral` per
/// path-shaped string literal. Interpolated strings (`"#{x}"`) are skipped.
pub fn extract_ruby_path_literals(root: Node<'_>, source: &[u8]) -> Vec<RawPathLiteral> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "string" {
            if let Some(rpl) = build_raw_path_literal(n, source) {
                out.push(rpl);
            }
            // Don't descend into string children — the whole node is consumed.
            continue;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    out
}

fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
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
