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
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
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
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
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
                field_reads: Vec::new(),
                owner_class: Some("ConfigLoader".to_string()),
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

/// Stands for: a top-level declaration's uid is `uid::compute` of its stored
/// fields, and a member's uid hashes its container's uid (`nested_uid`,
/// C1-identity.md "Declaration identity: fields and invariants"): `load` sits
/// inside `ConfigLoader`.
#[test]
fn uid_matches_direct_compute() {
    let mut b = GraphBuilder::new();
    b.add_graph(make_local_graph());
    let g = b.build();
    let pool = g.string_pool.as_slice();
    let uid_of = |name: &str| {
        let hits: Vec<u64> = g
            .nodes
            .iter()
            .filter(|n| n.kind != NodeKind::File && n.name.resolve(pool) == name)
            .map(|n| n.uid)
            .collect();
        assert_eq!(hits.len(), 1, "one `{name}`");
        hits[0]
    };
    let path = "src/stable.rs";
    for (kind, name) in [
        (NodeKind::Function, "parseConfig"),
        (NodeKind::Class, "ConfigLoader"),
    ] {
        assert_eq!(
            uid_of(name),
            ecp_core::uid::compute(kind, path, None, name),
            "top-level `{name}`"
        );
    }
    assert_eq!(
        uid_of("load"),
        ecp_core::contracts::identity::nested_uid(
            uid_of("ConfigLoader"),
            NodeKind::Method,
            Some("ConfigLoader"),
            "load"
        ),
        "the member `load` hashes its class"
    );
}
