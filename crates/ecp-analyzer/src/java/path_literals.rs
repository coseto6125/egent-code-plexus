//! Java-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` handles `string_literal` and `text_block`
//! (Java 15+ triple-quote) nodes. Invoked from
//! `receiver_types::extract_java_calls_and_path_literals` so a single
//! DFS handles both call attribution and path-literal collection.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_ext_change_callee, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let raw_bytes = &source[str_node.start_byte()..str_node.end_byte()];
    let raw = std::str::from_utf8(raw_bytes).ok()?;
    let value = strip_quotes(raw)?;
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
