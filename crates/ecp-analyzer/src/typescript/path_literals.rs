//! TypeScript-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` dispatches on `string` (single/double-quoted)
//! and static `template_string` nodes; dynamic template literals (`${}`)
//! return `None`. Invoked from
//! `receiver_types::extract_ts_calls_and_path_literals` so a single DFS
//! handles both call attribution and path-literal collection.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_ext_change_callee, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    match node.kind() {
        "string" => build_from_string(node, source),
        "template_string" => build_from_template(node, source),
        _ => None,
    }
}

fn build_from_string(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let raw_bytes = &source[str_node.start_byte()..str_node.end_byte()];
    let raw = std::str::from_utf8(raw_bytes).ok()?;
    let value = strip_quotes(raw)?;
    emit(str_node, value, source)
}

fn build_from_template(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    // Skip template literals with substitution (`${…}`).
    {
        let mut c = str_node.walk();
        for child in str_node.children(&mut c) {
            if child.kind() == "template_substitution" {
                return None;
            }
        }
    }

    let raw_bytes = &source[str_node.start_byte()..str_node.end_byte()];
    let raw = std::str::from_utf8(raw_bytes).ok()?;
    // Template string outer bytes are backticks.
    let value = strip_backticks(raw)?;
    emit(str_node, value, source)
}

fn emit(str_node: Node<'_>, value: &str, source: &[u8]) -> Option<RawPathLiteral> {
    let callee = enclosing_callee(str_node, source);
    // Sink-override: ext-change callees accept short non-path-shaped values.
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

/// Strip surrounding single or double quotes from a TS `string` node.
/// Returns `None` if the shape is malformed.
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let quote_char = *bytes.first()?;
    if quote_char != b'"' && quote_char != b'\'' {
        return None;
    }
    let body_end = bytes.len().checked_sub(1)?;
    if bytes[body_end] != quote_char {
        return None;
    }
    std::str::from_utf8(&bytes[1..body_end]).ok()
}

/// Strip surrounding backticks from a TS `template_string` node.
fn strip_backticks(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    if bytes.first() != Some(&b'`') {
        return None;
    }
    let body_end = bytes.len().checked_sub(1)?;
    if bytes[body_end] != b'`' {
        return None;
    }
    std::str::from_utf8(&bytes[1..body_end]).ok()
}

/// Climb from a string literal to find the enclosing `call_expression` and
/// resolve its callee name. Returns `None` when not a direct argument.
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    if parent.kind() != "arguments" {
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
    match function.kind() {
        "identifier" => Some(text.to_string()),
        "member_expression" => {
            let prop = function.child_by_field_name("property")?;
            std::str::from_utf8(&source[prop.start_byte()..prop.end_byte()])
                .ok()
                .map(str::to_string)
        }
        _ => Some(text.to_string()),
    }
}

/// Climb from a string literal to find the innermost enclosing function/method
/// and class. Returns `(function_name, owner_class)`.
///
/// TS function-boundary nodes: `function_declaration`, `method_definition`,
/// `function_expression`, `arrow_function`. Class boundary: `class_declaration`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_declaration" | "method_definition" | "function_expression"
                if function_name.is_none() =>
            {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "arrow_function" if function_name.is_none() => {
                // Arrow functions are anonymous; attempt to get name from parent
                // variable_declarator if present.
                if let Some(parent) = n.parent() {
                    if parent.kind() == "variable_declarator" {
                        if let Some(name_node) = parent.child_by_field_name("name") {
                            function_name = std::str::from_utf8(
                                &source[name_node.start_byte()..name_node.end_byte()],
                            )
                            .ok()
                            .map(str::to_string);
                        }
                    }
                }
            }
            "class_declaration" | "class" if owner.is_none() => {
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
