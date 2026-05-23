//! Java-side extractor for `RawPathLiteral` entries. Walks `string_literal`
//! and `text_block` (Java 15+ triple-quote) nodes, filters via
//! `path_literal::is_path_shaped`, classifies via `path_literal::classify_sink`,
//! and resolves enclosing method / class via parent-chain walk.
//!
//! No interpolated strings in plain Java — every `string_literal` is static.
//! `text_block` (`"""..."""`) may span multiple lines but is also always static.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

/// Walk the Java tree and emit one `RawPathLiteral` per path-shaped string.
pub fn extract_java_path_literals(root: Node<'_>, source: &[u8]) -> Vec<RawPathLiteral> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "string_literal" | "text_block") {
            if let Some(rpl) = build_raw_path_literal(n, source) {
                out.push(rpl);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    out
}

fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let raw_bytes = &source[str_node.start_byte()..str_node.end_byte()];
    let raw = std::str::from_utf8(raw_bytes).ok()?;
    let value = strip_quotes(raw)?;
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

/// Strip surrounding quotes from a Java `string_literal` or `text_block`.
/// - `"foo"` → `foo`
/// - `"""foo"""` → `foo` (text_block; leading/trailing newlines trimmed by Java spec)
///
/// Returns `None` when shape is malformed.
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    // text_block: starts with `"""` and ends with `"""`
    if bytes.starts_with(b"\"\"\"") && bytes.ends_with(b"\"\"\"") && bytes.len() >= 6 {
        let inner = &raw[3..raw.len() - 3];
        // Strip the mandatory newline immediately after opening `"""`
        let trimmed = inner.trim_matches(['\n', '\r', ' ', '\t'].as_slice());
        return Some(trimmed);
    }
    // Normal string_literal: `"..."` — strip one quote on each side
    if bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"') && bytes.len() >= 2 {
        let body_start = 1;
        let body_end = bytes.len() - 1;
        if body_end >= body_start {
            return std::str::from_utf8(&bytes[body_start..body_end]).ok();
        }
    }
    None
}

/// Climb the AST to find the enclosing `method_invocation` or
/// `object_creation_expression` callee name. Java call chains look like:
///   `method_invocation > argument_list > string_literal`
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    if parent.kind() != "argument_list" {
        return None;
    }
    let call = parent.parent()?;
    match call.kind() {
        "method_invocation" => {
            // `name` field holds the method identifier
            let name_node = call.child_by_field_name("name")?;
            std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                .ok()
                .map(str::to_string)
        }
        "object_creation_expression" => {
            // Constructor call — use the type name as callee (e.g. `File`, `Paths`)
            let type_node = call.child_by_field_name("type")?;
            std::str::from_utf8(&source[type_node.start_byte()..type_node.end_byte()])
                .ok()
                .map(str::to_string)
        }
        _ => None,
    }
}

/// Climb the AST to find the enclosing method/constructor and enclosing class.
/// Returns `(method_name, class_name)`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "method_declaration" | "constructor_declaration" if function_name.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "class_declaration" | "record_declaration" | "interface_declaration"
                if owner.is_none() =>
            {
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
