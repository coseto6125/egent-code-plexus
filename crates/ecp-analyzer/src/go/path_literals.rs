//! Go-side extractor for `RawPathLiteral` entries. Walks the file's
//! `interpreted_string_literal` / `raw_string_literal` nodes, filters via
//! `path_literal::is_path_shaped`, classifies the call-context sink via
//! `path_literal::classify_sink`, and resolves the enclosing function /
//! method (or `None` for package-level literals).
//!
//! Go has two string forms:
//!   `"foo"` — interpreted (C-style escapes, `\n` etc.)
//!   `` `foo` `` — raw (no escape processing, literal backslash)
//!
//! Runs as a side pass after the main `queries.scm` capture loop; reuses
//! the already-parsed `tree` so cost is one extra DFS walk, no re-parse.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

/// Walk the Go tree-sitter tree and emit one `RawPathLiteral` per
/// path-shaped string literal. Returns an empty Vec when no candidates
/// satisfy `is_path_shaped`.
pub fn extract_go_path_literals(root: Node<'_>, source: &[u8]) -> Vec<RawPathLiteral> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "interpreted_string_literal" | "raw_string_literal"
        ) {
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

/// Strip surrounding quotes from a Go string literal.
/// `"foo"` → `foo` (interpreted_string_literal, C-style escapes)
/// `` `foo` `` → `foo` (raw_string_literal, no escapes)
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    if bytes.first() == Some(&b'`') {
        // raw string: backtick-delimited
        let last = bytes.last()?;
        if *last != b'`' {
            return None;
        }
        return std::str::from_utf8(&bytes[1..bytes.len() - 1]).ok();
    }
    // interpreted string: double-quote delimited
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let body_end = bytes.len().checked_sub(1)?;
    if body_end == 0 || bytes.get(body_end) != Some(&b'"') {
        return None;
    }
    std::str::from_utf8(&bytes[1..body_end]).ok()
}

/// Climb the AST from a string literal to find the enclosing
/// `call_expression` and resolve its callee name. In Go the argument list
/// is `argument_list`; the call target lives in the `function` field of
/// `call_expression`.
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
    match function.kind() {
        "identifier" => Some(text.to_string()),
        // `os.ReadFile` → selector_expression, field is `ReadFile`
        "selector_expression" => {
            let field = function.child_by_field_name("field")?;
            std::str::from_utf8(&source[field.start_byte()..field.end_byte()])
                .ok()
                .map(str::to_string)
        }
        _ => Some(text.to_string()),
    }
}

/// Climb the AST from a string literal to find the enclosing
/// `function_declaration` (free function) or `method_declaration`.
/// Returns `(function_name, receiver_type)`.
///
/// For methods, `receiver_type` is extracted from the receiver parameter list
/// and used as `enclosing_owner` so `(Dog) name()` and `(Cat) name()` don't
/// collide in the post-process pass.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "function_declaration" => {
                let name_node = n.child_by_field_name("name");
                let fn_name = name_node.and_then(|nn| {
                    std::str::from_utf8(&source[nn.start_byte()..nn.end_byte()])
                        .ok()
                        .map(str::to_string)
                });
                return (fn_name, None);
            }
            "method_declaration" => {
                let name_node = n.child_by_field_name("name");
                let fn_name = name_node.and_then(|nn| {
                    std::str::from_utf8(&source[nn.start_byte()..nn.end_byte()])
                        .ok()
                        .map(str::to_string)
                });
                let owner = super::receiver_types::receiver_type_from_method_decl(n, source);
                return (fn_name, owner);
            }
            _ => {}
        }
        cur = n.parent();
    }
    (None, None)
}
