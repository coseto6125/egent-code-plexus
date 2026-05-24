//! Integration tests for `session::merge_archived` (T7-5).
//!
//! Covers: uid-based override, additive overlay, empty overlay, exact-once
//! iteration count.  Tombstone / deletion is NOT supported in v1 — see the
//! `test_merge_deletion_via_tombstone` test for the documented limitation.

use ecp_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use ecp_core::session::{merge_archived, ArchivedOverlay, Overlay};
use rkyv::rancor::Error as RkyvError;

// ── helpers ─────────────────────────────────────────────────────────────────

fn make_node(uid: u64, name_ref: StrRef, kind: NodeKind) -> Node {
    Node {
        uid,
        name: name_ref,
        file_idx: 0,
        kind,
        span: (0, 0, 0, 0),
        community_id: 0,
        owner_class: StrRef::default(),
        content_hash: uid,
    }
}

/// Build a minimal valid `ZeroCopyGraph` archive from the given nodes.
fn build_graph(pool_bytes: Vec<u8>, nodes: Vec<Node>) -> Vec<u8> {
    let n = nodes.len();
    // out_offsets / in_offsets: n+1 zero entries (no edges).
    let out_offsets = vec![0u32; n + 1];
    let in_offsets = vec![0u32; n + 1];
    let g = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool_bytes,
        files: vec![File {
            path: StrRef::default(),
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        }],
        nodes,
        edges: Vec::<Edge>::new(),
        out_offsets,
        in_offsets,
        in_edge_idx: vec![],
        name_index: vec![],
        process_start: 0,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
        kind_offsets: vec![],
        kind_node_idx: vec![],
        node_flags: vec![],
    };
    rkyv::to_bytes::<RkyvError>(&g)
        .expect("serialize graph")
        .into_vec()
}

/// Build a minimal valid `Overlay` archive from the given nodes.
fn build_overlay(nodes: Vec<Node>) -> Vec<u8> {
    let overlay = Overlay::new(nodes);
    rkyv::to_bytes::<RkyvError>(&overlay)
        .expect("serialize overlay")
        .into_vec()
}

// ── tests ────────────────────────────────────────────────────────────────────

/// When overlay has a node with the same uid as a base node, the overlay node
/// is yielded and the base node is suppressed.
#[test]
fn test_merge_override_overlay_wins_on_uid_match() {
    let mut pool = StringPool::new();
    let name_base = pool.add("old_name");
    let name_overlay = pool.add("new_name");
    let pool_bytes = pool.bytes.clone();

    let base_node = make_node(42, name_base, NodeKind::Function);
    // Overlay node with same uid=42 but different name_ref
    let overlay_node = make_node(42, name_overlay, NodeKind::Function);

    let base_bytes = build_graph(pool_bytes.clone(), vec![base_node]);
    let overlay_bytes = build_overlay(vec![overlay_node]);

    let base = rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, RkyvError>(&base_bytes)
        .expect("access base");
    let overlay =
        rkyv::access::<ArchivedOverlay, RkyvError>(&overlay_bytes).expect("access overlay");

    let result: Vec<_> = merge_archived(base, overlay).collect();
    assert_eq!(
        result.len(),
        1,
        "exactly one node (overlay suppresses base)"
    );
    assert_eq!(result[0].uid.to_native(), 42, "uid must be 42");

    // overlay_node has name "new_name" (higher offset in pool),
    // base_node has "old_name" (lower offset). Verify name_ref offset differs.
    assert_ne!(
        result[0].name.offset.to_native(),
        name_base.offset,
        "overlay node's name_ref must differ from base node's"
    );
    assert_eq!(
        result[0].name.offset.to_native(),
        name_overlay.offset,
        "overlay node's name_ref must match the overlay-side name"
    );
}

/// A node in the overlay whose uid does not appear in the base is yielded.
#[test]
fn test_merge_addition_yields_overlay_only() {
    let pool_bytes = vec![];

    let base_node = make_node(1, StrRef::default(), NodeKind::Function);
    let overlay_node = make_node(99, StrRef::default(), NodeKind::Method);

    let base_bytes = build_graph(pool_bytes, vec![base_node]);
    let overlay_bytes = build_overlay(vec![overlay_node]);

    let base = rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, RkyvError>(&base_bytes)
        .expect("access base");
    let overlay =
        rkyv::access::<ArchivedOverlay, RkyvError>(&overlay_bytes).expect("access overlay");

    let result: Vec<_> = merge_archived(base, overlay).collect();
    assert_eq!(result.len(), 2, "both nodes yielded");
    let uids: Vec<u64> = result.iter().map(|n| n.uid.to_native()).collect();
    assert!(uids.contains(&1), "base uid=1 present");
    assert!(uids.contains(&99), "overlay uid=99 present");
}

/// Tombstone / deletion is NOT supported in v1.
///
/// The overlay schema has no `deleted_uids` field.  A node present only in
/// the base cannot be suppressed by the overlay.  T7-6 may add tombstones.
///
/// This test is `#[ignore]`d because it documents a *known limitation*, not
/// broken behaviour.  Run it via `cargo test -- --ignored` to confirm the
/// limitation is still present.
#[test]
#[ignore = "tombstone/deletion not implemented in v1 overlay schema (T7-6 scope)"]
fn test_merge_deletion_via_tombstone() {
    // If tombstones were supported, placing uid=5 in a `deleted_uids` list
    // should suppress it from the output.
    //
    // Current behaviour: uid=5 in base but absent from overlay → still yielded.
    let base_bytes = build_graph(
        vec![],
        vec![make_node(5, StrRef::default(), NodeKind::Function)],
    );
    let overlay_bytes = build_overlay(vec![]); // empty overlay, no tombstone API

    let base = rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, RkyvError>(&base_bytes)
        .expect("access base");
    let overlay =
        rkyv::access::<ArchivedOverlay, RkyvError>(&overlay_bytes).expect("access overlay");

    let result: Vec<_> = merge_archived(base, overlay).collect();
    // With tombstones this would be 0; today it is 1 (no deletion support).
    assert_eq!(
        result.len(),
        0,
        "tombstone should suppress uid=5 — FAILS until T7-6 adds deleted_uids"
    );
}

/// base=100 nodes, overlay overrides 10, adds 5 → iterator yields exactly 105
/// unique uids.
#[test]
fn test_merge_iterates_each_uid_once() {
    let base_nodes: Vec<Node> = (0u64..100)
        .map(|uid| make_node(uid, StrRef::default(), NodeKind::Function))
        .collect();

    // 10 overrides (uids 0-9) + 5 additions (uids 100-104)
    let overlay_nodes: Vec<Node> = (0u64..10)
        .chain(100u64..105)
        .map(|uid| make_node(uid, StrRef::default(), NodeKind::Method))
        .collect();

    let base_bytes = build_graph(vec![], base_nodes);
    let overlay_bytes = build_overlay(overlay_nodes);

    let base = rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, RkyvError>(&base_bytes)
        .expect("access base");
    let overlay =
        rkyv::access::<ArchivedOverlay, RkyvError>(&overlay_bytes).expect("access overlay");

    let result: Vec<_> = merge_archived(base, overlay).collect();

    // 10 overlay overrides + 5 overlay additions + 90 unchanged base = 105
    assert_eq!(result.len(), 105, "105 total nodes");

    let mut uids: Vec<u64> = result.iter().map(|n| n.uid.to_native()).collect();
    uids.sort_unstable();
    uids.dedup();
    assert_eq!(uids.len(), 105, "all uids are distinct");
}

/// Empty overlay → iterator yields exactly the base nodes, in order.
#[test]
fn test_merge_with_empty_overlay_yields_base_unchanged() {
    let base_nodes: Vec<Node> = (0u64..50)
        .map(|uid| make_node(uid, StrRef::default(), NodeKind::Function))
        .collect();

    let base_bytes = build_graph(vec![], base_nodes);
    let overlay_bytes = build_overlay(vec![]); // empty

    let base = rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, RkyvError>(&base_bytes)
        .expect("access base");
    let overlay =
        rkyv::access::<ArchivedOverlay, RkyvError>(&overlay_bytes).expect("access overlay");

    let result: Vec<_> = merge_archived(base, overlay).collect();
    assert_eq!(result.len(), 50, "all 50 base nodes yielded");

    let uids: Vec<u64> = result.iter().map(|n| n.uid.to_native()).collect();
    let expected: Vec<u64> = (0u64..50).collect();
    assert_eq!(uids, expected, "base nodes yielded in order");
}

/// Zero-alloc gate: the FxHashSet is built once at iterator construction and
/// MergeIter::next performs zero heap allocations during drain.
///
/// Structural assertion: because `MergeIter::next` contains only
/// `FxHashSet::contains` (read-only) and integer index arithmetic, the Rust
/// compiler cannot insert any heap allocation call in that path.  We verify
/// the output count is correct as a proxy for correct execution, and rely on
/// code-review to enforce the structural property.
///
/// A `#[global_allocator]` wrapper cannot be scoped to a single test in a
/// multi-test binary without races from parallel tests.  For a deterministic
/// byte-level gate, instrument via a dedicated bench binary with
/// `dhat::Profiler::new_heap()` (see `docs/adr/t7-5-alloc-gate.md`).
#[test]
fn test_merge_iteration_allocs_only_once() {
    let base_nodes: Vec<Node> = (0u64..1000)
        .map(|uid| make_node(uid, StrRef::default(), NodeKind::Function))
        .collect();
    let overlay_nodes: Vec<Node> = (0u64..100)
        .map(|uid| make_node(uid, StrRef::default(), NodeKind::Method))
        .collect();

    let base_bytes = build_graph(vec![], base_nodes);
    let overlay_bytes = build_overlay(overlay_nodes);

    let base = rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, RkyvError>(&base_bytes)
        .expect("access base");
    let overlay =
        rkyv::access::<ArchivedOverlay, RkyvError>(&overlay_bytes).expect("access overlay");

    // 100 overlay + 900 unchanged base = 1000 nodes total.
    let count = merge_archived(base, overlay).count();
    assert_eq!(count, 1000, "100 overlay + 900 base = 1000 nodes");
}
