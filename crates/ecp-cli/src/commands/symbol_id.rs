//! FQN (fully-qualified-name) helpers for `ecp inspect` and `ecp impact`.
//!
//! ## Why derive owner from HasMethod edges, not a stored field?
//!
//! `Node` in the archived graph has no `owner_class` field — the field lives
//! on `RawNode` (intermediate parse stage) and is used to emit `HasMethod`
//! edges during graph construction but is not persisted. Walking the
//! incoming edge set for `HasMethod` is the correct zero-copy approach and
//! avoids a schema bump.

use ecp_core::graph::{ArchivedRelType, ArchivedZeroCopyGraph};

/// Resolve the owner class name for a node by scanning its incoming edges for
/// a `HasMethod` or `HasProperty` edge from a class-kind node.
///
/// Returns the owning class name when found, `None` for module-level symbols.
/// The lookup is O(in-degree of node), typically 1 for methods, 0 for free
/// functions — never a bottleneck in practice.
pub fn resolve_owner_class(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> Option<&str> {
    let in_start = graph.in_offsets[node_idx].to_native() as usize;
    let in_end = graph.in_offsets[node_idx + 1].to_native() as usize;
    for i in in_start..in_end {
        let edge_idx = graph.in_edge_idx[i].to_native() as usize;
        let edge = &graph.edges[edge_idx];
        if matches!(
            edge.rel_type,
            ArchivedRelType::HasMethod | ArchivedRelType::HasProperty
        ) {
            let src_idx = edge.source.to_native() as usize;
            return Some(graph.nodes[src_idx].name.resolve(&graph.string_pool));
        }
    }
    None
}

/// Format a fully-qualified name from an optional owner class and a bare name.
///
/// - `Some("Foo"), "validate"` → `"Foo.validate"`
/// - `None, "validate"` → `"validate"`
pub fn format_fqn(owner: Option<&str>, name: &str) -> String {
    match owner {
        Some(o) if !o.is_empty() => format!("{o}.{name}"),
        _ => name.to_owned(),
    }
}

/// Parse a `--name` / `--target` argument into an optional owner prefix and
/// bare symbol name.
///
/// - `"Foo.validate"` → `(Some("Foo"), "validate")`
/// - `"validate"` → `(None, "validate")`
///
/// Splits on the FIRST `.` only so that namespaced names like
/// `"pkg.Foo.validate"` are treated as owner=`"pkg.Foo"`, name=`"validate"`.
/// Callers that only have single-level classes will never see that case, but
/// the convention is stable.
pub fn split_fqn_target(s: &str) -> (Option<&str>, &str) {
    match s.rfind('.') {
        Some(dot_pos) => (Some(&s[..dot_pos]), &s[dot_pos + 1..]),
        None => (None, s),
    }
}
