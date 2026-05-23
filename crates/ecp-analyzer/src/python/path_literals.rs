//! Python-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` is invoked from
//! `receiver_types::extract_python_calls_and_path_literals` so a single
//! DFS handles both call attribution and path-literal collection.
//!
//! Strips surrounding quotes (including raw, byte, triple-quote, and
//! prefix combos), skips f-strings (contain `interpolation` children),
//! filters via `path_literal::is_path_shaped`, classifies the
//! call-context sink via `path_literal::classify_sink`, and resolves the
//! enclosing function / class.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::framework_helpers::strip_python_string_quotes;
use crate::path_literal::{classify_sink, is_ext_change_callee, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
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
    let value = strip_python_string_quotes(raw)?;
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

/// Climb from a string literal to find the enclosing `call` and resolve its
/// callee name. Python AST shape for `Path("x.json").read_text()`:
/// ```text
/// call (outer — read_text())
///   attribute (Path("x.json").read_text)
///     call (inner — Path(...))
///       identifier "Path"
///       argument_list
///         string "x.json"    ← str_node
///     "."
///     identifier "read_text"
///   argument_list ()
/// ```
///
/// FU-2026-05-23-023 — when the inner call is chained via an `attribute`
/// node that is the `function` child of an outer `call`, promote to the
/// terminal method name if it's in the high-confidence read/write list.
/// Falls back to the inner call's callee (e.g. `Path`) otherwise.
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    if parent.kind() != "argument_list" {
        return None;
    }
    let inner_call = parent.parent()?;
    if inner_call.kind() != "call" {
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

/// Walk outward from `inner_call` through one `attribute` wrapper to find the
/// outer chained `call`, and return its method name.
fn terminal_chained_callee(inner_call: Node<'_>, source: &[u8]) -> Option<String> {
    let attr = inner_call.parent()?;
    if attr.kind() != "attribute" {
        return None;
    }
    let outer = attr.parent()?;
    if outer.kind() != "call" {
        return None;
    }
    // `attribute`'s last child is the method `identifier`.
    let mut c = attr.walk();
    let mut method_node: Option<Node<'_>> = None;
    for child in attr.children(&mut c) {
        if child.kind() == "identifier" {
            method_node = Some(child);
        }
    }
    let m = method_node?;
    std::str::from_utf8(&source[m.start_byte()..m.end_byte()])
        .ok()
        .map(str::to_string)
}

/// Names that justify chain-terminal promotion. Each must match a
/// `classify_sink` HIGH-confidence entry. `write_text` / `write_bytes` /
/// `read_bytes` are pending addition to `path_literal::classify_sink` (parent
/// session batch); they are listed here so promotion fires once the table
/// is updated — adding them now causes no regression (classify_sink returns
/// Free for unknown names, same as the non-promoted fallback).
fn is_high_confidence_chain_terminal(name: &str) -> bool {
    matches!(
        name,
        // reads (pathlib.Path)
        "read_text" | "read_bytes"
        // writes
        | "write_text" | "write_bytes"
    )
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
