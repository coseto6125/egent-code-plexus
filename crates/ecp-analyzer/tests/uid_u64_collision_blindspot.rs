//! T1-5 D1 collision recovery: duplicate-uid symbols produce a `uid-collision`
//! BlindSpotRecord and are silently dropped rather than panicking or creating
//! two nodes with the same UID.
//!
//! A collision is forced by submitting two `RawNode` entries with identical
//! (kind, file_path, owner_class, name) — the four inputs to `uid::compute` —
//! but different span values. The second node must be absent from the built
//! graph and a `uid-collision` blind spot must be present.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;

fn make_collision_graph() -> LocalGraph {
    LocalGraph {
        file_path: "src/collision.rs".into(),
        content_hash: [1; 8],
        nodes: vec![
            // First definition — kept.
            RawNode {
                name: "duplicateFn".to_string(),
                kind: NodeKind::Function,
                span: (1, 0, 5, 0),
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                owner_class: None,
            },
            // Second definition — same (kind, path, owner, name) → same uid → collision.
            RawNode {
                name: "duplicateFn".to_string(),
                kind: NodeKind::Function,
                span: (10, 0, 15, 0), // different span, same hash inputs
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                owner_class: None,
            },
        ],
        documents: vec![],
        imports: vec![],
        routes: vec![],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        call_metas: vec![],
        raw_function_metas: vec![],
    }
}

#[test]
fn collision_drops_second_node_no_panic() {
    let mut b = GraphBuilder::new();
    b.add_graph(make_collision_graph());
    let g = b.build();
    let pool = g.string_pool.as_slice();

    // Only one node with name "duplicateFn" should survive.
    let dup_nodes: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name.resolve(pool) == "duplicateFn")
        .collect();
    assert_eq!(
        dup_nodes.len(),
        1,
        "expected exactly one 'duplicateFn' node, got {}",
        dup_nodes.len()
    );

    // No two nodes should share the same uid.
    let mut seen_uids = std::collections::HashSet::new();
    for node in g.nodes.iter() {
        assert!(
            seen_uids.insert(node.uid),
            "duplicate uid {} found in built graph",
            node.uid
        );
    }
}

#[test]
fn collision_emits_uid_collision_blind_spot() {
    let mut b = GraphBuilder::new();
    b.add_graph(make_collision_graph());
    let g = b.build();
    let pool = g.string_pool.as_slice();

    let collision_spots: Vec<_> = g
        .blind_spots
        .iter()
        .filter(|bs| bs.kind.resolve(pool) == "uid-collision")
        .collect();

    assert_eq!(
        collision_spots.len(),
        1,
        "expected exactly one uid-collision blind spot, got {}; all kinds: {:?}",
        collision_spots.len(),
        g.blind_spots
            .iter()
            .map(|bs| bs.kind.resolve(pool))
            .collect::<Vec<_>>()
    );

    let hint = collision_spots[0].hint.resolve(pool);
    assert!(
        hint.contains("uid-collision:"),
        "blind spot hint should start with 'uid-collision:', got: {hint}"
    );
    assert!(
        hint.contains("duplicateFn"),
        "hint should mention the colliding symbol name, got: {hint}"
    );
}
