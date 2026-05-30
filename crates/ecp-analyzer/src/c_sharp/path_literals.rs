//! C#-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` handles `string_literal`,
//! `verbatim_string_literal` (`@"..."`), and `raw_string_literal` (C# 11
//! `"""..."""`). Invoked from
//! `receiver_types::extract_csharp_calls_and_path_literals` so a single
//! DFS handles both call attribution and path-literal collection.
//!
//! `interpolated_string_expression` (`$"..."`) nodes are visited by the
//! merged walker but don't match here (different node kind), so no
//! emission for them — calls inside interpolations are still collected
//! since the walker always descends.

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

/// Strip surrounding quotes from a C# string literal.
///
/// - `"foo"` → `foo`  (standard string, C-style escapes)
/// - `@"foo"` → `foo`  (verbatim: no escape processing; literal backslashes)
/// - `"""foo"""` → `foo`  (raw string C# 11: variable number of `"` delimiters;
///   at minimum three on each side; content is fully literal)
///
/// For raw strings, tree-sitter captures the exact source bytes including the
/// delimiters. We strip the outer matching `"` run.
fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();

    // verbatim_string_literal: starts with `@"` and ends with `"`
    if bytes.starts_with(b"@\"") && bytes.ends_with(b"\"") && bytes.len() >= 3 {
        return std::str::from_utf8(&bytes[2..bytes.len() - 1]).ok();
    }

    // raw_string_literal (C# 11): starts and ends with 3+ consecutive `"`
    // Count the leading `"` run
    if bytes.len() >= 6 && bytes[0] == b'"' {
        let mut leading = 0usize;
        while leading < bytes.len() && bytes[leading] == b'"' {
            leading += 1;
        }
        if leading >= 3 {
            // Must end with the same count of `"`
            let mut trailing = 0usize;
            let len = bytes.len();
            while trailing < len && bytes[len - 1 - trailing] == b'"' {
                trailing += 1;
            }
            if trailing == leading && len >= leading + trailing {
                let inner = &raw[leading..len - trailing];
                return Some(inner.trim_matches(['\n', '\r', ' ', '\t'].as_slice()));
            }
        }
    }

    // Standard string_literal: `"..."`
    if bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"') && bytes.len() >= 2 {
        return std::str::from_utf8(&bytes[1..bytes.len() - 1]).ok();
    }

    None
}

/// Re-exported for use in receiver_types SQL extraction.
pub(super) fn strip_csharp_string_value(raw: &str) -> Option<&str> {
    strip_quotes(raw)
}

/// Re-exported for use in receiver_types SQL extraction.
pub(super) fn enclosing_symbol_and_owner_pub(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    enclosing_symbol_and_owner(str_node, source)
}

/// Climb the AST to find the enclosing invocation callee name.
/// C# call nodes: `invocation_expression > argument_list > argument > string_literal`
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = str_node.parent()?;
    // May be wrapped in `argument`
    let arg_list = if parent.kind() == "argument" {
        parent.parent()?
    } else {
        parent
    };
    if arg_list.kind() != "argument_list" {
        return None;
    }
    let call = arg_list.parent()?;
    if call.kind() != "invocation_expression" {
        return None;
    }
    // function field holds the callee expression
    let function = call.child_by_field_name("function")?;
    // For `File.ReadAllText` it's a `member_access_expression`; for `Foo()` an `identifier`
    match function.kind() {
        "member_access_expression" => {
            // `name` field is the trailing identifier
            let name = function.child_by_field_name("name")?;
            std::str::from_utf8(&source[name.start_byte()..name.end_byte()])
                .ok()
                .map(str::to_string)
        }
        _ => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
    }
}

/// Climb the AST to find the enclosing `method_declaration`,
/// `constructor_declaration`, or `local_function_statement` and the enclosing
/// `class_declaration`, `record_declaration`, or `struct_declaration`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "method_declaration" | "constructor_declaration" | "local_function_statement"
                if function_name.is_none() =>
            {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "class_declaration" | "record_declaration" | "struct_declaration"
                if owner.is_none() =>
            {
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
