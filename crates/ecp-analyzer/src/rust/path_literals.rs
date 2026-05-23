//! Rust-side extractor for `RawPathLiteral` entries. Walks the file's
//! `string_literal` / `raw_string_literal` nodes, filters via
//! `path_literal::is_path_shaped`, classifies the call-context sink via
//! `path_literal::classify_sink`, and resolves the enclosing function /
//! method (or `None` for module-level literals).
//!
//! Runs as a side pass after the main `queries.scm` capture loop completes
//! in `rust/parser.rs::parse_file`; reuses the already-parsed `tree` so
//! cost is one extra DFS walk over the same tree (no re-parse).

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

/// Walk the Rust tree-sitter `tree` and emit one `RawPathLiteral` per
/// path-shaped string literal. Returns an empty Vec when no candidates
/// satisfy `is_path_shaped`.
pub fn extract_rust_path_literals(root: Node<'_>, source: &[u8]) -> Vec<RawPathLiteral> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "string_literal" | "raw_string_literal") {
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

/// Raw (`r#...`) and byte-raw (`br#...`) variants need hash-count match for
/// closing delim; plain `b"..."` / `"..."` take the simpler boundary path.
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    if bytes.first() == Some(&b'b') {
        i += 1;
    }
    if bytes.get(i) == Some(&b'r') {
        i += 1;
        let hash_start = i;
        while bytes.get(i) == Some(&b'#') {
            i += 1;
        }
        let hash_count = i - hash_start;
        if bytes.get(i) != Some(&b'"') {
            return None;
        }
        let body_start = i + 1;
        let trail = hash_count + 1;
        let body_end = bytes.len().checked_sub(trail)?;
        if body_end < body_start || bytes.get(body_end) != Some(&b'"') {
            return None;
        }
        return std::str::from_utf8(&bytes[body_start..body_end]).ok();
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    let body_start = i + 1;
    let body_end = bytes.len().checked_sub(1)?;
    if body_end < body_start || bytes.get(body_end) != Some(&b'"') {
        return None;
    }
    std::str::from_utf8(&bytes[body_start..body_end]).ok()
}

/// Climb the AST from a string literal to find the enclosing `call_expression`
/// and resolve its callee name. Returns `None` when the literal is not an
/// argument of a call (e.g. const initialiser, let binding, format-string).
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

/// Reduce a `call_expression > function` subtree to its trailing identifier.
/// Mirrors the shape of `rust_callee_name` in `receiver_types.rs` but does
/// not consult LocalTypes — for sink classification only the receiver-less
/// name matters (`Path::new`, `.join`, `read_to_string`).
fn callee_name(function: Node<'_>, source: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(&source[function.start_byte()..function.end_byte()]).ok()?;
    match function.kind() {
        "identifier" => Some(text.to_string()),
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            std::str::from_utf8(&source[field.start_byte()..field.end_byte()])
                .ok()
                .map(str::to_string)
        }
        "scoped_identifier" | "generic_function" => Some(text.to_string()),
        _ => Some(text.to_string()),
    }
}

/// Climb the AST from a string literal to find the enclosing
/// `function_item` (free function) or method (function_item inside impl_item).
/// Returns `(function_name, owner_class)` — owner is `Some(ty)` for methods,
/// `None` for free functions / module-top-level / const initialisers.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_item" if function_name.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "impl_item" if owner.is_none() => {
                if let Some(ty_node) = n.child_by_field_name("type") {
                    owner = std::str::from_utf8(&source[ty_node.start_byte()..ty_node.end_byte()])
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
