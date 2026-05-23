//! Python-side extractor for `RawPathLiteral` entries. Walks `string` nodes,
//! strips surrounding quotes (including raw, byte, triple-quote, and prefix
//! combos), skips f-strings (contain `interpolation` children), filters via
//! `path_literal::is_path_shaped`, classifies the call-context sink via
//! `path_literal::classify_sink`, and resolves the enclosing function / class.
//!
//! Runs as a side pass over the already-parsed `tree` in `python/parser.rs`.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

/// Walk the Python tree and emit one `RawPathLiteral` per path-shaped string
/// literal. F-strings (containing `interpolation` children) are skipped.
pub fn extract_python_path_literals(root: Node<'_>, source: &[u8]) -> Vec<RawPathLiteral> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "string" {
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
    // Skip f-strings: they contain `interpolation` children.
    {
        let mut c = str_node.walk();
        for child in str_node.children(&mut c) {
            if child.kind() == "interpolation" {
                return None;
            }
        }
    }

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

/// Triple-quote + prefix combos (r/b/u/rb/br/f/fr/rf) handled; f-strings
/// are pre-filtered by the caller's `interpolation` child check.
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut i = 0;

    // Skip known prefix characters: r, b, u, f (and combinations), case-insensitive.
    while i < bytes.len() {
        match bytes[i] {
            b'r' | b'R' | b'b' | b'B' | b'u' | b'U' | b'f' | b'F' => i += 1,
            _ => break,
        }
    }

    // Determine quote char and triple-quote.
    let quote_char = *bytes.get(i)?;
    if quote_char != b'"' && quote_char != b'\'' {
        return None;
    }

    // Triple-quote check.
    if bytes.get(i + 1) == Some(&quote_char) && bytes.get(i + 2) == Some(&quote_char) {
        let body_start = i + 3;
        let body_end = bytes.len().checked_sub(3)?;
        if body_end < body_start {
            return None;
        }
        if bytes[body_end] != quote_char
            || bytes[body_end + 1] != quote_char
            || bytes[body_end + 2] != quote_char
        {
            return None;
        }
        return std::str::from_utf8(&bytes[body_start..body_end]).ok();
    }

    // Single-quote.
    let body_start = i + 1;
    let body_end = bytes.len().checked_sub(1)?;
    if body_end < body_start || bytes[body_end] != quote_char {
        return None;
    }
    std::str::from_utf8(&bytes[body_start..body_end]).ok()
}

/// Climb from a string literal to find the enclosing `call` and resolve its
/// callee name. Returns `None` when the literal is not a direct argument.
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    if parent.kind() != "argument_list" {
        return None;
    }
    let call = parent.parent()?;
    if call.kind() != "call" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    callee_name(function, source)
}

fn callee_name(function: Node<'_>, source: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(&source[function.start_byte()..function.end_byte()]).ok()?;
    match function.kind() {
        "identifier" => Some(text.to_string()),
        "attribute" => {
            let attr = function.child_by_field_name("attribute")?;
            std::str::from_utf8(&source[attr.start_byte()..attr.end_byte()])
                .ok()
                .map(str::to_string)
        }
        _ => Some(text.to_string()),
    }
}

/// Climb from a string literal to find the innermost enclosing
/// `function_definition` (free function or method) and `class_definition`
/// (owner). Returns `(function_name, owner_class)`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_definition" if function_name.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "class_definition" if owner.is_none() => {
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
