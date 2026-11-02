//! A class instance field assigned in more than one place — `self.x = …` in
//! `__init__` and again in another method (Python), `this.x = …` (JS/TS),
//! `@x = …` (Ruby), etc. — produces two RawNodes with identical
//! (kind=Property, path, owner, name), i.e. a uid collision. The second is
//! correctly deduped to a single node, but this is the most common OO idiom,
//! NOT a parser bug: the merge records no BlindSpot, never `uid-collision`.
//!
//! Regression for the false `uid-collision` BlindSpot + "traversal may be
//! incomplete" that `ecp inspect`/`ecp impact` reported for ordinary code
//! (e.g. CacheManager._refresh_task assigned in __init__ and warmup_bot).
//!
//! Language-neutral by construction: it drives `classify_collision` via raw
//! Property nodes, so it covers every OO language's field-reassignment pattern
//! at once (the dimension the 14-language rule asks for — the classification is
//! shared, not per-parser).

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;

fn property_node(name: &str, owner: &str, span: (u32, u32, u32, u32)) -> RawNode {
    RawNode {
        name: name.to_string(),
        kind: NodeKind::Property,
        span,
        is_exported: false,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: vec![],
        field_reads: Vec::new(),
        owner_class: Some(owner.to_string()),
        content_hash: 0,
    }
}

/// Two assignments of the same instance field within one class.
fn reassigned_field_graph(path: &str) -> LocalGraph {
    LocalGraph {
        file_path: path.into(),
        content_hash: [2; 8],
        nodes: vec![
            // `self._refresh_task = None` in __init__.
            property_node("_refresh_task", "CacheManager", (89, 8, 89, 40)),
            // `self._refresh_task = create_task(...)` in another method.
            property_node("_refresh_task", "CacheManager", (235, 8, 235, 60)),
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
fn reassigned_field_dedups_to_one_node() {
    let mut b = GraphBuilder::new();
    b.add_graph(reassigned_field_graph("src/manager.py"));
    let g = b.build();
    let pool = g.string_pool.as_slice();

    let fields: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name.resolve(pool) == "_refresh_task")
        .collect();
    assert_eq!(
        fields.len(),
        1,
        "the reassigned field must collapse to a single Property node"
    );
}

#[test]
fn reassigned_field_is_not_uid_collision() {
    let mut b = GraphBuilder::new();
    b.add_graph(reassigned_field_graph("src/manager.py"));
    let g = b.build();
    let pool = g.string_pool.as_slice();

    let kinds: Vec<&str> = g
        .blind_spots
        .iter()
        .map(|bs| bs.kind.resolve(pool))
        .collect();
    assert!(
        !kinds.contains(&"uid-collision"),
        "reassigning an instance field is normal OO, not a parser bug; \
         must not emit a uid-collision BlindSpot. Got kinds: {kinds:?}"
    );
}

/// Stands for: the fix narrows the records, it does not blanket-suppress
/// them. A C header field repeated under `#ifdef` branches merges the same
/// way and still records `ifdef-redef` (`classify_collision`).
#[test]
fn c_header_field_twin_still_reports_ifdef_redef() {
    let mut b = GraphBuilder::new();
    b.add_graph(reassigned_field_graph("include/manager.h"));
    let g = b.build();
    let pool = g.string_pool.as_slice();
    let kinds: Vec<&str> = g
        .blind_spots
        .iter()
        .map(|bs| bs.kind.resolve(pool))
        .collect();
    let count = |kind: &str| kinds.iter().filter(|&&k| k == kind).count();
    assert_eq!(
        (count("ifdef-redef"), count("uid-collision")),
        (1, 0),
        "{kinds:?}"
    );
}
