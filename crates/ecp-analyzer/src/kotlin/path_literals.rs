//! Kotlin-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` handles `string_literal` and
//! `multiline_string_literal` nodes; interpolated strings (`$x` /
//! `${expr}`) are filtered out via both an AST-child check and a raw-text
//! fallback. Invoked from
//! `receiver_types::extract_kotlin_calls_and_path_literals` so a single
//! DFS handles both call attribution and path-literal collection.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_ext_change_callee, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    // AST-child check: tree-sitter-kotlin exposes `$x` / `${expr}` as an
    // `interpolation` child of the string node.
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
    // Raw-text fallback: tree-sitter-kotlin doesn't always expose
    // `interpolation` as a child kind; reject any string containing `${`.
    if value.contains("${") {
        return None;
    }
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

/// Multiline `"""..."""` content is trimmed of surrounding whitespace since
/// Kotlin's source indentation leaks into the captured slice.
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
/// Kotlin tree-sitter shape for `File("x.json").readText()`:
/// ```text
/// call_expression                     (outer — readText() call)
///   navigation_expression
///     call_expression                 (inner — File(...) call)
///       simple_identifier "File"
///       call_suffix
///         value_arguments
///           value_argument
///             string_literal          ← str_node
///     navigation_suffix
///       simple_identifier "readText"
///   call_suffix (empty args)
/// ```
///
/// FU-2026-05-23-023 — for chained calls, promote the callee from the
/// inner constructor (`File`) to the outer terminal method (`readText`)
/// when that name is in the high-confidence read/write/ext-change list.
/// Constructor names alone classify as `sink:join|medium` — the LLM
/// can't tell read-only from write-only without the promotion.
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    // string_literal → value_argument → value_arguments
    let mut cur = str_node.parent()?;
    if cur.kind() == "value_argument" {
        cur = cur.parent()?;
    }
    if cur.kind() != "value_arguments" {
        return None;
    }
    // value_arguments may sit directly under call_expression (older grammar)
    // or under a `call_suffix` wrapper (current). Accept either.
    let mut parent = cur.parent()?;
    if parent.kind() == "call_suffix" {
        parent = parent.parent()?;
    }
    if parent.kind() != "call_expression" {
        return None;
    }
    let inner_call = parent;

    // Try chain-terminal promotion first; fall back to the inner call's
    // leading callee when the chain doesn't yield a whitelisted name.
    if let Some(terminal) = terminal_chained_callee(inner_call, source) {
        if is_high_confidence_chain_terminal(&terminal) {
            return Some(terminal);
        }
    }
    leading_callee_name(inner_call, source)
}

/// Walk outward from `inner_call` through one `navigation_expression`
/// wrapper to find the outer chained `call_expression`, and return its
/// method name. Returns `None` when the inner call isn't chained.
fn terminal_chained_callee(inner_call: Node<'_>, source: &[u8]) -> Option<String> {
    let nav = inner_call.parent()?;
    if nav.kind() != "navigation_expression" {
        return None;
    }
    let outer = nav.parent()?;
    if outer.kind() != "call_expression" {
        return None;
    }
    // navigation_expression's last child is `navigation_suffix` whose
    // last child is the method `simple_identifier`.
    let mut c = nav.walk();
    let mut method_node: Option<Node<'_>> = None;
    for child in nav.children(&mut c) {
        if child.kind() == "navigation_suffix" {
            method_node = Some(child);
        }
    }
    let suffix = method_node?;
    let mut c = suffix.walk();
    for child in suffix.children(&mut c) {
        if child.kind() == "simple_identifier" {
            return std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                .ok()
                .map(str::to_string);
        }
    }
    None
}

/// Names that justify chain-terminal promotion. Each must match a
/// `classify_sink` HIGH-confidence entry; otherwise the LLM gains nothing
/// from the override.
fn is_high_confidence_chain_terminal(name: &str) -> bool {
    matches!(
        name,
        // reads (Kotlin stdlib + java.io.File)
        "readText" | "readBytes" | "readLines"
        // writes
        | "writeText" | "writeBytes" | "appendText" | "appendBytes"
    )
}

/// Resolve the leading callee name for a Kotlin `call_expression`.
/// Mirrors the original logic — `navigation_expression` member ident,
/// otherwise raw text.
fn leading_callee_name(call: Node<'_>, source: &[u8]) -> Option<String> {
    let callee_node = call.child(0)?;
    match callee_node.kind() {
        "navigation_expression" => {
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
