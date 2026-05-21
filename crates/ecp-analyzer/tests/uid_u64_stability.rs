//! T1-5 stability: parsing the same source twice must yield identical u64 UIDs.
//!
//! Regression guard for any change that would make `uid::compute` non-deterministic
//! across runs (e.g. PRNG seeding, hash-seed rotation, pool offset drift).

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;

fn make_local_graph() -> LocalGraph {
    LocalGraph {
        file_path: "src/stable.rs".into(),
        content_hash: [42; 8],
        nodes: vec![
            RawNode {
                name: "parseConfig".to_string(),
                kind: NodeKind::Function,
                span: (1, 0, 5, 0),
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                owner_class: None,
            },
            RawNode {
                name: "ConfigLoader".to_string(),
                kind: NodeKind::Class,
                span: (10, 0, 30, 0),
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                owner_class: None,
            },
            RawNode {
                name: "load".to_string(),
                kind: NodeKind::Method,
                span: (12, 0, 20, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec!["parseConfig".to_string()],
                owner_class: Some("ConfigLoader".to_string()),
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
fn uid_is_identical_across_two_builds() {
    let build = || {
        let mut b = GraphBuilder::new();
        b.add_graph(make_local_graph());
        b.build()
    };

    let g1 = build();
    let g2 = build();

    // Collect (uid, name, kind) triples from both graphs and compare.
    let pool1 = g1.string_pool.as_slice();
    let pool2 = g2.string_pool.as_slice();

    let mut uids1: Vec<(u64, String, String)> = g1
        .nodes
        .iter()
        .map(|n| {
            (
                n.uid,
                n.name.resolve(pool1).to_string(),
                format!("{:?}", n.kind),
            )
        })
        .collect();
    let mut uids2: Vec<(u64, String, String)> = g2
        .nodes
        .iter()
        .map(|n| {
            (
                n.uid,
                n.name.resolve(pool2).to_string(),
                format!("{:?}", n.kind),
            )
        })
        .collect();

    uids1.sort();
    uids2.sort();

    assert_eq!(
        uids1, uids2,
        "UIDs differ between two identical builds — hash is non-deterministic"
    );
}

#[test]
fn uid_matches_direct_compute() {
    let mut b = GraphBuilder::new();
    b.add_graph(make_local_graph());
    let g = b.build();
    let pool = g.string_pool.as_slice();

    for node in g.nodes.iter() {
        if !matches!(
            node.kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Class
        ) {
            continue;
        }
        let name = node.name.resolve(pool);
        let file_path = g.files[node.file_idx as usize].path.resolve(pool);
        let owner = if node.owner_class.len == 0 {
            None
        } else {
            Some(node.owner_class.resolve(pool))
        };
        let expected = ecp_core::uid::compute(node.kind, file_path, owner, name);
        assert_eq!(
            node.uid, expected,
            "uid mismatch for {name}: builder produced {}, direct compute gives {expected}",
            node.uid
        );
    }
}
