//! Shared helpers for framework-aware parser captures.
//!
//! Three language parsers (python / rust / typescript) all need to:
//!   1. Convert tree-sitter `Node` start/end positions to our `(row, col, row, col)` span tuple.
//!   2. Test whether one span contains another.
//!   3. Find the innermost enclosing `Function` / `Method` `RawNode` that covers a given span.
//!
//! This module consolidates those helpers so each parser stays focused on its own
//! grammar quirks, not span arithmetic.

use gnx_core::analyzer::types::RawNode;
use gnx_core::graph::NodeKind;

pub type Span = (u32, u32, u32, u32);

/// Sentinel `source_name` for framework refs registered at module level
/// (e.g. Actix `#[get]` attribute macros, top-level Express `app.get(...)`).
pub const MODULE_LEVEL_SOURCE: &str = "<module>";

/// Extract `(start_row, start_col, end_row, end_col)` span from a tree-sitter node.
#[inline]
pub fn node_span(node: &tree_sitter::Node) -> Span {
    let s = node.start_position();
    let e = node.end_position();
    (s.row as u32, s.column as u32, e.row as u32, e.column as u32)
}

/// True iff `outer` (row,col,row,col) fully contains `inner`.
#[inline]
pub fn span_contains(outer: Span, inner: Span) -> bool {
    let (or1, oc1, or2, oc2) = outer;
    let (ir1, ic1, ir2, ic2) = inner;
    let starts_after = (or1, oc1) <= (ir1, ic1);
    let ends_before = (ir2, ic2) <= (or2, oc2);
    starts_after && ends_before
}

/// Area proxy (row-major byte count approximation) for picking the smallest enclosing span.
#[inline]
pub fn span_area(s: Span) -> u64 {
    let (r1, c1, r2, c2) = s;
    let dr = r2.saturating_sub(r1) as u64;
    let dc = c2 as u64 + 10_000u64.saturating_sub(c1 as u64);
    dr * 10_000 + dc
}

/// Find the innermost `Function`/`Method` `RawNode` that contains `inner_span`.
/// Returns the node's `name` clone, or `None` if no enclosing fn (module-level).
pub fn enclosing_function_name(nodes: &[RawNode], inner_span: Span) -> Option<String> {
    nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .filter(|n| span_contains(n.span, inner_span))
        .min_by_key(|n| span_area(n.span))
        .map(|n| n.name.clone())
}
