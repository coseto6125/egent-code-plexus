//! Swift-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` handles `line_string_literal` (`"..."`),
//! `multi_line_string_literal` (`"""..."""`), and `raw_string_literal`
//! (`#"..."#`). Interpolated forms (`interpolated_expression` child) are
//! filtered internally. Invoked from
//! `receiver_types::extract_swift_calls_and_path_literals` so a single
//! DFS handles both call attribution and path-literal collection.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_ext_change_callee, is_path_shaped, sink_reason};

pub(super) fn build_raw_path_literal(str_node: Node<'_>, source: &[u8]) -> Option<RawPathLiteral> {
    let value = extract_string_content(str_node, source)?;
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

/// Extract string content, skipping interpolated forms.
fn extract_string_content<'a>(str_node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    match str_node.kind() {
        "line_string_literal" | "multi_line_string_literal" => {
            // Skip any string with interpolated_expression children.
            let mut c = str_node.walk();
            for child in str_node.children(&mut c) {
                if child.kind() == "interpolated_expression" {
                    return None;
                }
            }
            // Extract content from `line_str_text` or `multi_line_str_text` children,
            // or strip quotes from the raw text.
            let mut c2 = str_node.walk();
            for child in str_node.children(&mut c2) {
                match child.kind() {
                    "line_str_text" | "multi_line_str_text" => {
                        return std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                            .ok();
                    }
                    _ => {}
                }
            }
            // Fallback: strip surrounding quotes from raw node text.
            let raw =
                std::str::from_utf8(&source[str_node.start_byte()..str_node.end_byte()]).ok()?;
            strip_quotes_swift(raw)
        }
        "raw_string_literal" => {
            // `#"..."#` or `##"..."##` — no interpolation possible.
            let raw =
                std::str::from_utf8(&source[str_node.start_byte()..str_node.end_byte()]).ok()?;
            strip_raw_string_swift(raw)
        }
        _ => None,
    }
}

/// Strip surrounding quotes from a Swift `"..."` or `"""..."""` string literal.
fn strip_quotes_swift(raw: &str) -> Option<&str> {
    // Multi-line: `"""..."""`
    if let Some(inner) = raw
        .strip_prefix("\"\"\"")
        .and_then(|s| s.strip_suffix("\"\"\""))
    {
        return Some(inner.trim_matches('\n'));
    }
    // Regular: `"..."`
    if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        return Some(&raw[1..raw.len() - 1]);
    }
    None
}

/// Strip surrounding `#"..."#` (or `##"..."##`) from a Swift raw string literal.
fn strip_raw_string_swift(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] == b'#' {
        i += 1;
    }
    let hash_count = i;
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    let body_start = i + 1;
    // Find the closing `"` followed by `hash_count` `#` characters.
    let closing: String = std::iter::once('"')
        .chain(std::iter::repeat_n('#', hash_count))
        .collect();
    let inner = std::str::from_utf8(&bytes[body_start..]).ok()?;
    let end_pos = inner.find(closing.as_str())?;
    Some(&inner[..end_pos])
}

/// Climb the AST to find the enclosing call expression callee in Swift.
///
/// Swift has two distinct idioms for path I/O, both routed via argument
/// labels rather than chain-terminal method names:
///
/// 1. **Labelled-argument constructors** — `String(contentsOfFile: "x")`,
///    `Data(contentsOf: url)`. Function is a bare `simple_identifier` (type
///    name); the meaningful part is the argument label.
///
/// 2. **Navigation chains with labelled args** — `"str".write(toFile: "x")`,
///    `data.write(toFile: "x")`. Function is a `navigation_expression`
///    whose trailing ident is the chained method (`write`); the labelled
///    argument still carries the read/write intent (`toFile` ⇒ file write
///    rather than a stream write).
///
/// In both cases we prefer the argument label when it resolves to a
/// `classify_sink` HIGH entry, otherwise fall back to the function text.
fn enclosing_callee(str_node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cur = str_node.parent();
    // Track the most recent value_argument we climb through so we can read
    // its label when we reach a call_expression.
    let mut enclosing_arg: Option<Node<'_>> = None;
    while let Some(n) = cur {
        match n.kind() {
            "value_argument" => {
                if enclosing_arg.is_none() {
                    enclosing_arg = Some(n);
                }
                cur = n.parent();
            }
            "call_expression" => {
                // Swift grammar has no named `function` field on call_expression;
                // child(0) is always the callee expression.
                let fn_node = n.child(0)?;
                // Promote the argument label when it resolves to a HIGH-confidence
                // file-op name — covers both labelled-arg constructors
                // (function is `simple_identifier`) and labelled-arg chains
                // (function is `navigation_expression`).
                if let Some(label) = enclosing_arg.and_then(|arg| arg_label(arg, source)) {
                    if is_high_confidence_label(&label) {
                        return Some(label);
                    }
                }
                let text =
                    std::str::from_utf8(&source[fn_node.start_byte()..fn_node.end_byte()]).ok()?;
                return Some(text.to_string());
            }
            "argument" | "value_arguments" | "call_suffix" | "tuple_expression"
            | "labeled_statement" => {
                cur = n.parent();
            }
            // Stop climbing past function bodies.
            "function_body" | "lambda_literal" | "source_file" => return None,
            _ => cur = n.parent(),
        }
    }
    None
}

/// Argument labels that justify promotion over the bare function callee.
/// Each must match a HIGH-confidence entry in `classify_sink`; the LLM
/// gains nothing from promotion if the label still classifies as Free.
fn is_high_confidence_label(label: &str) -> bool {
    matches!(label, "contentsOfFile" | "contentsOf" | "toFile")
}

/// Extract the label text from a `value_argument` node.
/// `value_argument` → `value_argument_label` → `simple_identifier`
fn arg_label(arg: Node<'_>, source: &[u8]) -> Option<String> {
    let mut c = arg.walk();
    for child in arg.children(&mut c) {
        if child.kind() == "value_argument_label" {
            let mut c2 = child.walk();
            for lc in child.children(&mut c2) {
                if lc.kind() == "simple_identifier" {
                    return std::str::from_utf8(&source[lc.start_byte()..lc.end_byte()])
                        .ok()
                        .map(str::to_string);
                }
            }
        }
    }
    None
}

/// Climb the AST from a string literal to find the enclosing
/// `function_declaration` or `init_declaration`, and the enclosing
/// `class_declaration`, `struct_declaration`, or `enum_declaration`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_declaration" | "protocol_function_declaration" if function_name.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "init_declaration" if function_name.is_none() => {
                function_name = Some("init".to_string());
            }
            "class_declaration" if owner.is_none() => {
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
