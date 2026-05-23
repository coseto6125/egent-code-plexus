//! C-side extractor for `RawPathLiteral` entries. Walks the file's
//! `string_literal` and `concatenated_string` nodes, filters via
//! `path_literal::is_path_shaped`, classifies the call-context sink via
//! `path_literal::classify_sink`, and resolves the enclosing function.
//!
//! C string forms handled:
//!   `"foo"`   — ordinary string literal
//!   `L"foo"`  — wide-char prefix (treated as plain)
//!   `u8"foo"` — UTF-8 prefix (treated as plain)
//!   `"foo" "bar"` — concatenated_string (adjacent string literals)
//!
//! Macro expansions are skipped at the string level: if a `string_literal`
//! node's parent is a `preproc_def` or `preproc_function_def` we emit it
//! only when it passes `is_path_shaped`, same as any other literal. True
//! macro-expansion call sites (where the literal text isn't visible in AST)
//! are not tracked — deferred to a later phase.
//!
//! Runs as a side pass after the main `queries.scm` capture loop; reuses
//! the already-parsed `tree` so cost is one extra DFS walk, no re-parse.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
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

/// Join the individual `string_literal` pieces of a C `concatenated_string`
/// (`"foo" "bar"` → `foobar`). Test the joined value once with `is_path_shaped`.
pub(super) fn build_concatenated(concat_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let mut joined = String::new();
    let mut cursor = concat_node.walk();
    for child in concat_node.children(&mut cursor) {
        if child.kind() == "string_literal" {
            let raw_bytes = &source[child.start_byte()..child.end_byte()];
            let raw = std::str::from_utf8(raw_bytes).ok()?;
            let piece = strip_quotes(raw)?;
            joined.push_str(piece);
        }
    }
    if joined.is_empty() || !is_path_shaped(&joined) {
        return None;
    }

    let callee = enclosing_callee(concat_node, source);
    let (kind, conf) = classify_sink(callee.as_deref());
    let reason = sink_reason(kind, conf);
    let (enclosing_symbol, enclosing_owner) = enclosing_symbol_and_owner(concat_node, source);

    let pos = concat_node.start_position();
    let end = concat_node.end_position();
    Some(RawPathLiteral {
        value: joined,
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

/// Strip surrounding quotes from a C `string_literal` capture text.
/// Handles `"foo"`, `L"foo"`, `u8"foo"`, `u"foo"`, `U"foo"`.
/// All prefix forms are treated as plain strings (value is the inner bytes).
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    // Skip known encoding prefixes: L, u8, u, U
    if bytes.get(i) == Some(&b'L') || bytes.get(i) == Some(&b'u') || bytes.get(i) == Some(&b'U') {
        if bytes.get(i + 1) == Some(&b'8') {
            i += 2; // u8
        } else {
            i += 1; // L, u, U
        }
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
/// argument of a call (e.g. variable initialiser, #define body).
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    // In C the argument list is `argument_list`
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
    Some(text.to_string())
}

/// Climb the AST from a string literal to find the enclosing
/// `function_definition`. For plain C there is no class concept; plain C
/// functions live at file scope so `enclosing_owner` is always `None`.
///
/// To resolve the function name we walk up to `function_definition` and then
/// drill into its `declarator` field to find the innermost `identifier`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    while let Some(n) = cur {
        if n.kind() == "function_definition" {
            let fn_name = fn_definition_name(n, source);
            return (fn_name, None);
        }
        cur = n.parent();
    }
    (None, None)
}

/// Extract the plain identifier name from a C `function_definition` node
/// by drilling into the `declarator` field chain.
///
/// Shapes handled:
///   `int foo(…) { … }`        → declarator = function_declarator(identifier)
///   `int *foo(…) { … }`       → declarator = pointer_declarator(function_declarator(identifier))
fn fn_definition_name(fn_def: Node<'_>, source: &[u8]) -> Option<String> {
    let declarator = fn_def.child_by_field_name("declarator")?;
    innermost_identifier(declarator, source)
}

/// Recursively drill through `pointer_declarator` / `function_declarator`
/// wrappers to find the innermost `identifier`.
fn innermost_identifier<'a>(node: Node<'a>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
            .ok()
            .map(str::to_string),
        "function_declarator" | "pointer_declarator" | "array_declarator" => {
            let decl = node.child_by_field_name("declarator")?;
            innermost_identifier(decl, source)
        }
        _ => None,
    }
}
