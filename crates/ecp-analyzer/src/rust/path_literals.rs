//! Rust-side helpers for `RawPathLiteral` extraction. Public entry point
//! `build_raw_path_literal` is invoked from
//! `receiver_types::extract_rust_calls_and_path_literals` so a single DFS
//! over the parsed tree visits both `call_expression` (for the call graph)
//! and `string_literal` / `raw_string_literal` (for path literals).
//!
//! Path-shape filter: `path_literal::is_path_shaped`.
//! Sink classifier: `path_literal::classify_sink` + `sink_reason`.

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
///
/// FU-2026-05-23-023 — when the inner call is the constructor leg of a
/// method chain (`File::open("x").unwrap().read_to_string(...)`), walk
/// outward through `field_expression / call_expression` wrappers and
/// promote to the terminal method name when it's a HIGH-confidence
/// file-op. `.unwrap()` / `.expect()` / `.ok()` are treated as
/// transparent adapters: the walk skips through them to reach the real
/// terminal read / write call.
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    if parent.kind() != "arguments" {
        return None;
    }
    let inner_call = parent.parent()?;
    if inner_call.kind() != "call_expression" {
        return None;
    }

    if let Some(terminal) = terminal_chained_callee(inner_call, source) {
        if is_high_confidence_chain_terminal(&terminal) {
            return Some(terminal);
        }
    }

    let function = inner_call.child_by_field_name("function")?;
    callee_name(function, source)
}

/// Walk outward from `inner_call` through chained `field_expression /
/// call_expression` pairs. `.unwrap()` / `.expect()` / `.ok()` are
/// transparent adapters — the walk skips through them. Returns the
/// first HIGH-confidence file-op method name encountered, or `None`.
fn terminal_chained_callee(inner_call: Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = inner_call;
    loop {
        // Each chain step is `field_expression` wrapping `current`,
        // then `call_expression` wrapping the `field_expression`.
        let field = current.parent()?;
        if field.kind() != "field_expression" {
            return None;
        }
        let outer_call = field.parent()?;
        if outer_call.kind() != "call_expression" {
            return None;
        }
        let field_id = field.child_by_field_name("field")?;
        let method = std::str::from_utf8(&source[field_id.start_byte()..field_id.end_byte()])
            .ok()?
            .to_string();
        if is_high_confidence_chain_terminal(&method) {
            return Some(method);
        }
        if !is_transparent_adapter(&method) {
            return None;
        }
        current = outer_call;
    }
}

/// Method names in the chain that classify_sink resolves to a HIGH
/// read/write/ext-change. Promotion is only useful when there's a
/// concrete winner downstream.
fn is_high_confidence_chain_terminal(name: &str) -> bool {
    matches!(
        name,
        // reads (std::io::Read + fs convenience)
        "read_to_string" | "read_to_end" | "read_text"
        // writes
        | "write_all" | "write_text"
        // ext-change
        | "with_extension" | "with_file_name" | "set_extension" | "set_file_name"
    )
}

/// Methods that are transparent in a chain — the walk should skip over
/// them rather than terminating. `Result::unwrap` etc. don't change the
/// effective sink.
fn is_transparent_adapter(name: &str) -> bool {
    matches!(
        name,
        "unwrap" | "expect" | "ok" | "unwrap_or" | "unwrap_or_default" | "unwrap_or_else"
    )
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
