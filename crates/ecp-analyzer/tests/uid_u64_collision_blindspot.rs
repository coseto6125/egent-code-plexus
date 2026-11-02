//! T1-5 D1 collision recovery: a merged repeat produces a `uid-collision`
//! BlindSpotRecord and becomes a tombstone rather than panicking or creating
//! two nodes with the same UID.
//!
//! A merged repeat is forced by submitting two `RawNode` entries of a kind
//! that merges repeats (`uid::merges_repeats`) with identical (kind,
//! file_path, owner_class, name) — the four inputs to `uid::compute` — but
//! different span values. The second node must lose its name and a
//! `uid-collision` blind spot must be present. A repeat of any other kind is
//! a later node of its own (design uid §2.3) and reports nothing.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;

/// Two declarations of `kind` named `name` in `src/collision.py`.
fn make_collision_graph(kind: NodeKind, name: &str) -> LocalGraph {
    LocalGraph {
        file_path: "src/collision.py".into(),
        content_hash: [1; 8],
        nodes: vec![
            // First definition — kept.
            RawNode {
                name: name.to_string(),
                kind,
                span: (1, 0, 5, 0),
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            },
            // Second definition — same (kind, path, owner, name) → same uid → collision.
            RawNode {
                name: name.to_string(),
                kind,
                span: (10, 0, 15, 0), // different span, same hash inputs
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            },
        ],
        documents: vec![],
        imports: vec![],
        routes: vec![],
        route_receivers: None,
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        path_literals: None,
        sql_refs: None,
        call_metas: vec![],
        raw_function_metas: vec![],
        export_aliases: None,
        parsed_language: ecp_core::contracts::routing::ParsedLanguage::ABSENT,
    }
}

fn build(kind: NodeKind, name: &str) -> ecp_core::graph::ZeroCopyGraph {
    let mut b = GraphBuilder::new();
    b.add_graph(make_collision_graph(kind, name));
    b.build()
}

fn kinds(g: &ecp_core::graph::ZeroCopyGraph) -> Vec<&str> {
    let pool = g.string_pool.as_slice();
    g.blind_spots
        .iter()
        .map(|bs| bs.kind.resolve(pool))
        .collect()
}

/// Stands for: a merged repeat keeps one named node; the second slot is a
/// tombstone with a uid of its own.
#[test]
fn collision_drops_second_node_no_panic() {
    let g = build(NodeKind::Variable, "duplicateVar");
    let pool = g.string_pool.as_slice();

    let dup_nodes: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name.resolve(pool) == "duplicateVar")
        .collect();
    assert_eq!(
        dup_nodes.len(),
        1,
        "expected exactly one 'duplicateVar' node, got {}",
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
    let g = build(NodeKind::Variable, "duplicateVar");
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
        kinds(&g)
    );

    let hint = collision_spots[0].hint.resolve(pool);
    assert!(
        hint.contains("uid-collision:"),
        "blind spot hint should start with 'uid-collision:', got: {hint}"
    );
    assert!(
        hint.contains("duplicateVar"),
        "hint should mention the colliding symbol name, got: {hint}"
    );
}

/// Stands for: a Python `def f` twice is a redefinition, a later node of
/// its own with its name under `overload(f, 1)`, so nothing is lost and no
/// `uid-collision` BlindSpot reports it. Red before 1b: the second `def`
/// was a tombstone with a BlindSpot.
#[test]
fn redefined_function_later_node_no_uid_collision_blind_spot() {
    let g = build(NodeKind::Function, "f");
    let pool = g.string_pool.as_slice();
    let named: Vec<u64> = g
        .nodes
        .iter()
        .filter(|n| n.name.resolve(pool) == "f")
        .map(|n| n.uid)
        .collect();
    let f = ecp_core::uid::compute(NodeKind::Function, "src/collision.py", None, "f");
    assert_eq!(named, [f, ecp_core::uid::overload(f, 1)]);
    assert!(!kinds(&g).contains(&"uid-collision"), "{:?}", kinds(&g));
}
