//! Ruby receiver-type binding for method call sites.
//!
//! Ruby's dynamic dispatch makes full receiver inference undecidable for
//! arbitrary `obj.method` calls.  This module handles the two statically
//! resolvable cases:
//!
//! - `self.method` inside a `class Foo … end` or `module Foo … end` block
//!   → receiver = the enclosing class/module name (`Foo.method`)
//! - `Constant.method` — a constant-named receiver (singleton method on a
//!   known class) → receiver = `Constant.method`
//!
//! All other `expr.method` calls fall back to the bare method name (existing
//! behaviour via `extract_calls`).

use super::path_literals::build_raw_path_literal;
use crate::calls::attach_to_enclosing;
use ecp_core::analyzer::types::{RawNode, RawPathLiteral};
use ecp_core::graph::NodeKind;
use tree_sitter::Node;

/// Enclosing class/module context built from the parsed node list.
struct ClassContext {
    /// `(start_row, end_row, class_name)`
    entries: Vec<(u32, u32, String)>,
}

impl ClassContext {
    fn from_nodes(nodes: &[RawNode]) -> Self {
        let entries = nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Class))
            .map(|n| (n.span.0, n.span.2, n.name.clone()))
            .collect();
        Self { entries }
    }

    /// Return the name of the innermost class/module enclosing `line`.
    fn enclosing_name(&self, line: u32) -> Option<&str> {
        let mut best: Option<(&str, u32)> = None;
        for (start, end, name) in &self.entries {
            if *start <= line && line <= *end {
                let width = end - start;
                if best.is_none_or(|(_, w)| width < w) {
                    best = Some((name.as_str(), width));
                }
            }
        }
        best.map(|(n, _)| n)
    }
}

/// Walk the Ruby AST once, attaching callees to enclosing nodes with
/// receiver-type binding (`self.method` / `Constant.method`) and
/// collecting path-shaped `string` literals. Interpolated strings are
/// filtered inside `build_raw_path_literal`.
pub fn extract_ruby_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> Vec<RawPathLiteral> {
    let ctx = ClassContext::from_nodes(nodes);
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call" => {
                if let Some(callee) = ruby_callee(n, source, &ctx) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "string" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    path_literals
}

/// Resolve the callee for a Ruby `call` node.
///
/// Tree-sitter-ruby `call` fields:
/// - `receiver`: the object before `.` / `&.` / `::` (optional for bare calls)
/// - `method`:   the method name identifier
fn ruby_callee(call: Node<'_>, source: &[u8], ctx: &ClassContext) -> Option<String> {
    let method_node = call.child_by_field_name("method")?;
    let method_name = method_node.utf8_text(source).ok()?;

    let line = call.start_position().row as u32;

    match call.child_by_field_name("receiver") {
        None => {
            // Bare call with no receiver — emit the bare method name.
            Some(method_name.to_string())
        }
        Some(receiver) => {
            if let Some(inferred_type) = infer_receiver_type(receiver, source, ctx, line) {
                Some(format!("{inferred_type}.{method_name}"))
            } else {
                // Bare name fallback.
                Some(method_name.to_string())
            }
        }
    }
}

fn infer_receiver_type(
    node: Node<'_>,
    source: &[u8],
    ctx: &ClassContext,
    line: u32,
) -> Option<String> {
    match node.kind() {
        "self" => ctx.enclosing_name(line).map(|s| s.to_string()),
        "constant" => node.utf8_text(source).ok().map(|s| s.to_string()),
        "call" => {
            let method_node = node.child_by_field_name("method")?;
            let method_name = method_node.utf8_text(source).ok()?;

            match method_name {
                "new" | "create" | "create!" | "find" | "find_by" | "find_by!" | "where"
                | "includes" | "joins" | "first" | "last" | "order" | "limit" | "offset" => {
                    if let Some(inner) = node.child_by_field_name("receiver") {
                        infer_receiver_type(inner, source, ctx, line)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}
