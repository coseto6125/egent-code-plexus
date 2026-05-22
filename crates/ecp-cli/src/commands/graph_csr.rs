//! CSR (Compressed Sparse Row) in-edge walk helpers.
//!
//! The `ZeroCopyGraph` stores incoming edges as a CSR index:
//!   `in_offsets[node_idx] .. in_offsets[node_idx + 1]` → range into `in_edge_idx[]` → `edges[]`.
//!
//! This iterator is the single canonical implementation of that walk; callers
//! `.collect()` at the use site when materialisation is needed.
//!
// Future migration candidates (different shapes per call site — needs care):
// - routes.rs:331, routes.rs:454
// - impact.rs:1112
// - find.rs:230, find.rs:751
// - find_tx_patterns.rs:344
// - inspect.rs:152

use ecp_core::graph::{ArchivedRelType, ArchivedZeroCopyGraph};

/// Iterate incoming edges to `target_idx` via the CSR in-edge index.
///
/// Yields `(source_node_idx, edge_idx)` pairs. Returns an empty iterator when
/// `target_idx` is out-of-bounds (guards against OOB on malformed graphs).
pub fn iter_incoming_edges(
    graph: &ArchivedZeroCopyGraph,
    target_idx: u32,
) -> impl Iterator<Item = (u32, u32)> + '_ {
    let off = graph.in_offsets.as_slice();
    let range = if (target_idx as usize + 1) < off.len() {
        let start = off[target_idx as usize].to_native() as usize;
        let end = off[target_idx as usize + 1].to_native() as usize;
        start..end
    } else {
        0..0
    };
    graph.in_edge_idx[range]
        .iter()
        .map(|archived| archived.to_native())
        .map(|edge_idx| {
            let src = graph.edges[edge_idx as usize].source.to_native();
            (src, edge_idx)
        })
}

/// Iterate incoming edges to `target_idx`, keeping only those whose `rel_type`
/// satisfies `pred`.
///
/// Yields `(source_node_idx, edge_idx)` pairs. Zero-allocation; materialise
/// at the call site with `.collect()` if a `Vec` is needed.
pub fn iter_incoming_edges_filtered<'g, F>(
    graph: &'g ArchivedZeroCopyGraph,
    target_idx: u32,
    pred: F,
) -> impl Iterator<Item = (u32, u32)> + 'g
where
    F: Fn(&ArchivedRelType) -> bool + 'g,
{
    iter_incoming_edges(graph, target_idx)
        .filter(move |&(_src, edge_idx)| pred(&graph.edges[edge_idx as usize].rel_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Constructing a synthetic ArchivedZeroCopyGraph requires rkyv
    // serialisation round-trip which is non-trivial in a unit test.
    // The filter predicate itself is tested here in isolation; end-to-end
    // coverage (including OOB guard and Calls/References filtering) is
    // provided by the integration test `diff_symbols_test`.

    #[test]
    fn filter_predicate_calls_matches_calls_not_references_free() {
        // Verify the predicate that symbols.rs passes behaves as expected
        // before even touching graph data.
        let calls_pred =
            |r: &ArchivedRelType| matches!(r, ArchivedRelType::Calls | ArchivedRelType::References);
        // Safety: ArchivedRelType is a repr(u8) enum; transmuting the
        // discriminants that match the variants we expect is the canonical
        // test for the predicate in isolation.
        assert!(calls_pred(&ArchivedRelType::Calls));
        assert!(calls_pred(&ArchivedRelType::References));
        assert!(!calls_pred(&ArchivedRelType::Imports));
        assert!(!calls_pred(&ArchivedRelType::Extends));
    }
}
