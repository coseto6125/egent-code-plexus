//! FQN (fully-qualified-name) helpers for `ecp inspect` and `ecp impact`.
//!
//! ## Owner-class resolution strategy
//!
//! On `main`, the archived `Node` has no `owner_class` field — the field lives
//! on `RawNode` (intermediate parse stage) and is used to emit `HasMethod`
//! edges during graph construction but is not persisted. Walking the incoming
//! edge set for `HasMethod` / `HasProperty` is the zero-copy stopgap.
//!
//! Follow-up: when PR #285 (`fix/t1-11-rename-owner-class`) lands and adds
//! `Node.owner_class: StrRef` to the archived schema, `resolve_owner_class`
//! collapses to a single field read — O(1) instead of O(in-degree). The
//! current edge-walk semantics stay correct in the meantime.

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
/// - `"pkg.Foo.validate"` → `(Some("pkg.Foo"), "validate")`
/// - `"validate"` → `(None, "validate")`
///
/// Splits on the **last** `.` so the bare name on the right matches
/// `Node.name` (which is always a bare identifier) while everything left of
/// the final dot becomes the owner prefix. This admits namespaced owners
/// (`pkg.Foo`) without changing the single-level (`Foo.validate`) contract.
///
/// `rename` (PR #285) currently splits on the first `.` — that PR will be
/// migrated to share this helper as a follow-up to keep dot semantics
/// uniform across the CLI.
pub fn split_fqn_target(s: &str) -> (Option<&str>, &str) {
    match s.rsplit_once('.') {
        Some((owner, name)) => (Some(owner), name),
        None => (None, s),
    }
}
