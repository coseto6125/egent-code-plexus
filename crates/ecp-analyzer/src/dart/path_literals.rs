//! Dart-side extractor for `RawPathLiteral` entries. Walks the file's
//! string literal nodes, filters interpolated strings, filters via
//! `path_literal::is_path_shaped`, classifies the call-context sink via
//! `path_literal::classify_sink`, and resolves the enclosing function / class.
//!
//! Dart string literal node kinds in tree-sitter-dart:
//!   `string_literal` — wraps the actual content. May contain
//!     `template_substitution` children (`$x` / `${expr}`) — skip those.
//!   Raw strings (`r'...'` / `r"..."`) do not use template_substitution.
//!
//! The string content lives in `string_literal_quote` or direct text children.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

/// Walk the Dart tree-sitter tree and emit one `RawPathLiteral` per
/// path-shaped string literal. Interpolated strings (`"$x"`) are skipped.
pub fn extract_dart_path_literals(root: Node<'_>, source: &[u8]) -> Vec<RawPathLiteral> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "string_literal" {
            if let Some(rpl) = build_raw_path_literal(n, source) {
                out.push(rpl);
            }
            // Don't descend — whole string node is consumed.
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

/// Extract string content from a Dart `string_literal` node.
/// Skips any string with `template_substitution` children (interpolated).
fn extract_string_content<'a>(str_node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    // Check for template_substitution (interpolation) children.
    let mut c = str_node.walk();
    for child in str_node.children(&mut c) {
        if child.kind() == "template_substitution" {
            return None;
        }
    }

    // Get the raw text of the string_literal and strip quotes.
    let raw = std::str::from_utf8(&source[str_node.start_byte()..str_node.end_byte()]).ok()?;
    strip_quotes_dart(raw)
}

/// Strip surrounding quotes from a Dart string literal. Handles:
///   `'...'` single-quoted
///   `"..."` double-quoted
///   `r'...'` raw single-quoted (no escape processing)
///   `r"..."` raw double-quoted
///   `'''...'''` triple single-quoted
///   `"""..."""` triple double-quoted
fn strip_quotes_dart(raw: &str) -> Option<&str> {
    // Raw strings: strip leading `r`.
    let s = raw.strip_prefix('r').unwrap_or(raw);

    // Triple-quoted forms.
    if let Some(inner) = s.strip_prefix("'''").and_then(|s| s.strip_suffix("'''")) {
        return Some(inner);
    }
    if let Some(inner) = s
        .strip_prefix("\"\"\"")
        .and_then(|s| s.strip_suffix("\"\"\""))
    {
        return Some(inner);
    }

    let bytes = s.as_bytes();
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

/// Climb the AST to find the enclosing call expression callee in Dart.
/// Dart call shapes:
///   `invocation_expression` → `function` field
///   method chain: parent is `selector` or argument list
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cur = str_node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "invocation_expression" => {
                // The function being called is the first child of invocation_expression.
                let fn_node = n.child_by_field_name("function").or_else(|| n.child(0))?;
                let text =
                    std::str::from_utf8(&source[fn_node.start_byte()..fn_node.end_byte()]).ok()?;
                return Some(text.to_string());
            }
            "arguments" => {
                cur = n.parent();
            }
            // Stop past function boundaries.
            "function_body" | "block" | "program" => return None,
            _ => cur = n.parent(),
        }
    }
    None
}

/// Climb the AST from a string literal to find the enclosing function and class.
/// Dart grammar: functions are in `function_declaration`, methods in `method_declaration`,
/// classes in `class_definition`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_declaration" if function_name.is_none() => {
                if let Some(sig_node) = n.child_by_field_name("signature") {
                    if let Some(name_node) = sig_node.child_by_field_name("name") {
                        function_name = std::str::from_utf8(
                            &source[name_node.start_byte()..name_node.end_byte()],
                        )
                        .ok()
                        .map(str::to_string);
                    }
                }
                // Also try direct name child for top-level functions.
                if function_name.is_none() {
                    if let Some(name_node) = n.child_by_field_name("name") {
                        function_name = std::str::from_utf8(
                            &source[name_node.start_byte()..name_node.end_byte()],
                        )
                        .ok()
                        .map(str::to_string);
                    }
                }
            }
            "method_declaration" if function_name.is_none() => {
                // Method: find the function_signature > name inside it.
                if let Some(sig) = n.child_by_field_name("signature") {
                    // sig is method_signature, look for function_signature child.
                    let mut sc = sig.walk();
                    for child in sig.children(&mut sc) {
                        if child.kind() == "function_signature" {
                            if let Some(name_node) = child.child_by_field_name("name") {
                                function_name = std::str::from_utf8(
                                    &source[name_node.start_byte()..name_node.end_byte()],
                                )
                                .ok()
                                .map(str::to_string);
                                break;
                            }
                        }
                    }
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
