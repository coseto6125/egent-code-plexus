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

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, extract_ruby_string_value,
};
use crate::calls::{attach_to_enclosing, range_width, start_pos, Pos, PosRange};
use ecp_core::analyzer::types::{RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use tree_sitter::Node;

/// Enclosing class/module context built from the parsed node list.
struct ClassContext {
    /// `((start, end) positions, class_name)`
    entries: Vec<(PosRange, String)>,
}

impl ClassContext {
    fn from_nodes(nodes: &[RawNode]) -> Self {
        let entries = nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Class))
            .map(|n| (((n.span.0, n.span.1), (n.span.2, n.span.3)), n.name.clone()))
            .collect();
        Self { entries }
    }

    /// Return the name of the innermost class/module enclosing `pos`.
    fn enclosing_name(&self, pos: Pos) -> Option<&str> {
        let mut best: Option<(&str, (u32, i64))> = None;
        for ((start, end), name) in &self.entries {
            if *start <= pos && pos <= *end {
                let width = range_width((*start, *end));
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
/// collecting path-shaped `string` literals and SQL-shaped string literals.
/// Interpolated strings are filtered inside `build_raw_path_literal`.
pub fn extract_ruby_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let ctx = ClassContext::from_nodes(nodes);
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call" => {
                if let Some(callee) = ruby_callee(n, source, &ctx) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "string" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                if let Some(value) = extract_ruby_string_value(n, source) {
                    if let Some(sql_ref) = crate::sql_literal::try_sql_ref(
                        value,
                        n,
                        enclosing_symbol_and_owner_pub(n, source),
                    ) {
                        sql_refs.push(sql_ref);
                    }
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    (path_literals, sql_refs)
}

/// Resolve the callee for a Ruby `call` node.
///
/// Tree-sitter-ruby `call` fields:
/// - `receiver`: the object before `.` / `&.` / `::` (optional for bare calls)
/// - `method`:   the method name identifier
fn ruby_callee(call: Node<'_>, source: &[u8], ctx: &ClassContext) -> Option<String> {
    let method_node = call.child_by_field_name("method")?;
    let method_name = method_node.utf8_text(source).ok()?;

    let pos = start_pos(call);

    match call.child_by_field_name("receiver") {
        None => {
            // Bare call with no receiver — emit the bare method name.
            Some(method_name.to_string())
        }
        Some(receiver) => {
            // Only a construction keeps a scoped receiver (`Shop::Item.new`):
            // other `A::B.m` calls resolve by the bare method name, and
            // qualifying them drops the Method edges that name binds.
            let receiver_type = match (receiver.kind(), method_name) {
                ("scope_resolution", "new") => receiver
                    .utf8_text(source)
                    .ok()
                    .map(|s| s.trim_start_matches("::").to_string()),
                _ => infer_receiver_type(receiver, source, ctx, pos),
            };
            if let Some(inferred_type) = receiver_type {
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
    pos: Pos,
) -> Option<String> {
    match node.kind() {
        "self" => ctx.enclosing_name(pos).map(|s| s.to_string()),
        "constant" => node.utf8_text(source).ok().map(|s| s.to_string()),
        "call" => {
            let method_node = node.child_by_field_name("method")?;
            let method_name = method_node.utf8_text(source).ok()?;

            match method_name {
                "new" | "create" | "create!" | "find" | "find_by" | "find_by!" | "where"
                | "includes" | "joins" | "first" | "last" | "order" | "limit" | "offset" => {
                    if let Some(inner) = node.child_by_field_name("receiver") {
                        infer_receiver_type(inner, source, ctx, pos)
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
