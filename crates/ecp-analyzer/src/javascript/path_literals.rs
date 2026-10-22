//! JavaScript-side helpers for `RawPathLiteral` extraction. Entry point
//! `build_raw_path_literal` dispatches on `string` (single/double-quoted)
//! and static `template_string` nodes; dynamic template literals (`${}`)
//! return `None`. Invoked from
//! `receiver_types::extract_js_calls_and_path_literals` so a single DFS
//! handles both call attribution and path-literal collection.

use ecp_core::analyzer::types::RawPathLiteral;
use tree_sitter::Node;

use crate::path_literal::{classify_sink, is_ext_change_callee, is_path_shaped, sink_reason};

/// Names already known by the call walk, borrowed until a literal is emitted.
#[derive(Clone, Copy, Default)]
pub(super) struct LiteralContext<'s> {
    pub symbol: Option<&'s str>,
    pub owner: Option<&'s str>,
    callee: Option<&'s str>,
}

impl<'s> LiteralContext<'s> {
    pub fn for_children(self, node: Node<'_>, parent: Option<Node<'_>>, source: &'s [u8]) -> Self {
        let name = |node: Node<'_>| node.child_by_field_name("name")?.utf8_text(source).ok();
        let mut context = self;
        context.callee = None;
        match node.kind() {
            "function_declaration" | "method_definition" | "function_expression" => {
                context.symbol = name(node).or(context.symbol);
            }
            "arrow_function" => {
                context.symbol = parent
                    .filter(|parent| parent.kind() == "variable_declarator")
                    .and_then(name)
                    .or(context.symbol);
            }
            "class_declaration" | "class" => context.owner = name(node).or(context.owner),
            "arguments" => {
                context.callee = parent
                    .filter(|parent| parent.kind() == "call_expression")
                    .and_then(|call| call.child_by_field_name("function"))
                    .and_then(|function| callee_name(function, source));
            }
            _ => {}
        }
        context
    }
}

pub(super) fn build_raw_path_literal(
    node: Node<'_>,
    source: &[u8],
    context: LiteralContext<'_>,
) -> Option<RawPathLiteral> {
    match node.kind() {
        "string" => build_from_string(node, source, context),
        "template_string" => build_from_template(node, source, context),
        _ => None,
    }
}

fn build_from_string(
    str_node: Node<'_>,
    source: &[u8],
    context: LiteralContext<'_>,
) -> Option<RawPathLiteral> {
    let raw_bytes = &source[str_node.start_byte()..str_node.end_byte()];
    let raw = std::str::from_utf8(raw_bytes).ok()?;
    let value = strip_quotes(raw)?;
    emit(str_node, value, context)
}

fn build_from_template(
    str_node: Node<'_>,
    source: &[u8],
    context: LiteralContext<'_>,
) -> Option<RawPathLiteral> {
    // Skip template literals with substitution (`${…}`).
    {
        let mut c = str_node.walk();
        for child in str_node.children(&mut c) {
            if child.kind() == "template_substitution" {
                return None;
            }
        }
    }

    let raw_bytes = &source[str_node.start_byte()..str_node.end_byte()];
    let raw = std::str::from_utf8(raw_bytes).ok()?;
    let value = strip_backticks(raw)?;
    emit(str_node, value, context)
}

fn emit(str_node: Node<'_>, value: &str, context: LiteralContext<'_>) -> Option<RawPathLiteral> {
    // Sink-override: ext-change callees accept short non-path-shaped values.
    if !is_path_shaped(value) && !is_ext_change_callee(context.callee) {
        return None;
    }
    let (kind, conf) = classify_sink(context.callee);
    let reason = sink_reason(kind, conf);

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
        enclosing_symbol: context.symbol.map(str::to_string),
        enclosing_owner: context.owner.map(str::to_string),
        sink_reason: reason,
    })
}

/// Strip surrounding single or double quotes from a JS `string` node.
///
/// Re-exported as `pub(super)` for use in receiver_types SQL extraction.
pub(super) fn strip_js_string_value(raw: &str) -> Option<&str> {
    strip_quotes(raw)
}

fn strip_quotes(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let quote_char = *bytes.first()?;
    if quote_char != b'"' && quote_char != b'\'' {
        return None;
    }
    let body_end = bytes.len().checked_sub(1)?;
    if bytes[body_end] != quote_char {
        return None;
    }
    std::str::from_utf8(&bytes[1..body_end]).ok()
}

/// Strip surrounding backticks from a JS `template_string` node.
fn strip_backticks(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    if bytes.first() != Some(&b'`') {
        return None;
    }
    let body_end = bytes.len().checked_sub(1)?;
    if bytes[body_end] != b'`' {
        return None;
    }
    std::str::from_utf8(&bytes[1..body_end]).ok()
}

fn callee_name<'s>(function: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let text = std::str::from_utf8(&source[function.start_byte()..function.end_byte()]).ok()?;
    match function.kind() {
        "identifier" => Some(text),
        "member_expression" => {
            let prop = function.child_by_field_name("property")?;
            std::str::from_utf8(&source[prop.start_byte()..prop.end_byte()]).ok()
        }
        _ => Some(text),
    }
}
