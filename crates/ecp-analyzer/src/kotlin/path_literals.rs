//! Kotlin-side extractor for `RawPathLiteral` entries. Walks `string_literal`
//! nodes (which contain `string_content` children) and `multiline_string_literal`
//! nodes, filters via `path_literal::is_path_shaped`, classifies via
//! `path_literal::classify_sink`, and resolves enclosing function/class via
//! parent-chain walk.
//!
//! Interpolated strings — `string_literal` nodes whose children include an
//! `interpolation` node — are skipped entirely because their runtime value is
//! dynamic and would produce noise.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

/// Walk the Kotlin tree and emit one `RawPathLiteral` per path-shaped string.
pub fn extract_kotlin_path_literals(root: Node<'_>, source: &[u8]) -> Vec<RawPathLiteral> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "string_literal" | "multiline_string_literal") {
            // Skip strings containing interpolation — dynamic value.
            if has_interpolation(n) {
                // Still push children to walk nested expressions
                let mut c = n.walk();
                for child in n.children(&mut c) {
                    stack.push(child);
                }
                continue;
            }
            if let Some(rpl) = build_raw_path_literal(n, source) {
                out.push(rpl);
            }
        } else {
            let mut c = n.walk();
            for child in n.children(&mut c) {
                stack.push(child);
            }
        }
    }
    out
}

/// Returns true when the string_literal / multiline_string_literal contains
/// an `interpolation` child (e.g. `"$x"`, `"${expr}"`).
fn has_interpolation(str_node: Node<'_>) -> bool {
    let mut c = str_node.walk();
    for child in str_node.children(&mut c) {
        if child.kind() == "interpolation" {
            return true;
        }
    }
    false
}

fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let raw_bytes = &source[str_node.start_byte()..str_node.end_byte()];
    let raw = std::str::from_utf8(raw_bytes).ok()?;
    let value = strip_quotes(raw)?;
    // Fallback interpolation check: tree-sitter-kotlin may not always expose
    // `interpolation` as a child kind; reject any string containing `${`.
    if value.contains("${") {
        return None;
    }
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

/// Strip surrounding quotes from a Kotlin string literal.
/// - `"foo"` → `foo`  (string_literal: the node text includes the quotes)
/// - `"""foo"""` → `foo`  (multiline_string_literal: triple-quote; literal backslashes)
///
/// Kotlin `string_literal` node text: `"<content>"` where content is the
/// concatenation of `string_content` and `escape_seq` child texts — tree-sitter
/// delivers the full source slice including quotes. We strip one level.
///
/// For `multiline_string_literal` (`"""..."""`), no escape sequences are
/// processed — backslashes are literal in triple-quoted strings.
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    // multiline_string_literal: `"""..."""`
    if bytes.starts_with(b"\"\"\"") && bytes.ends_with(b"\"\"\"") && bytes.len() >= 6 {
        let inner = &raw[3..raw.len() - 3];
        return Some(inner.trim_matches(['\n', '\r', ' ', '\t'].as_slice()));
    }
    // string_literal: `"..."`
    if bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"') && bytes.len() >= 2 {
        return std::str::from_utf8(&bytes[1..bytes.len() - 1]).ok();
    }
    None
}

/// Climb the AST to find the enclosing Kotlin call expression callee.
/// Kotlin call nodes: `call_expression > value_arguments > value_argument > string_literal`
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    // value_argument or string_literal may be nested in different ways
    let mut cur = str_node.parent()?;
    // Handle `value_argument` wrapper
    if cur.kind() == "value_argument" {
        cur = cur.parent()?;
    }
    if cur.kind() != "value_arguments" {
        return None;
    }
    let call = cur.parent()?;
    if call.kind() != "call_expression" {
        return None;
    }
    // The callee is the `callsuffix` or the leading expression child
    // In Kotlin grammar: `call_expression > (navigation_expression | simple_identifier) > call_suffix`
    // The first child of call_expression holds the callee
    let callee_node = call.child(0)?;
    // For `foo.bar(...)` it's a `navigation_expression`; for `bar(...)` an `identifier`
    match callee_node.kind() {
        "navigation_expression" => {
            // Last child of navigation_expression is the member name
            let count = callee_node.child_count();
            if count == 0 {
                return None;
            }
            let member = callee_node.child(count as u32 - 1)?;
            std::str::from_utf8(&source[member.start_byte()..member.end_byte()])
                .ok()
                .map(str::to_string)
        }
        _ => std::str::from_utf8(&source[callee_node.start_byte()..callee_node.end_byte()])
            .ok()
            .map(str::to_string),
    }
}

/// Climb the AST to find the enclosing `function_declaration` and enclosing
/// `class_declaration` / `object_declaration`.
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
                // Kotlin function name: `(simple_identifier)` child
                let mut c = n.walk();
                for child in n.children(&mut c) {
                    if child.kind() == "simple_identifier" {
                        if let Ok(s) =
                            std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                        {
                            function_name = Some(s.to_string());
                        }
                        break;
                    }
                }
            }
            "class_declaration" | "object_declaration" if owner.is_none() => {
                // Class/object name: `(type_identifier)` child
                let mut c = n.walk();
                for child in n.children(&mut c) {
                    if child.kind() == "type_identifier" {
                        if let Ok(s) =
                            std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                        {
                            owner = Some(s.to_string());
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
        cur = n.parent();
    }
    (function_name, owner)
}
