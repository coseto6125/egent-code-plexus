//! End-to-end verification of WHERE label-test predicate (Expr::HasLabel).
//!
//! Before this feature, `WHERE n:A OR n:B` failed at parse time with
//! "expected Return, found Some(Colon)" because parse_primary only handled
//! `ident.prop`, `ident(...)`, and bare `ident`. The OpenCypher
//! disjunction form is `n:A|B|C` (pipe, not OR) — this test pins both
//! single-label and pipe-disjoined behaviour.

use ecp_core::cypher;
use ecp_core::cypher::lexer::tokenize;
use ecp_core::cypher::parser::parse_query;
use ecp_core::graph::{
    ArchivedZeroCopyGraph, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use rkyv::rancor::Error;
use tempfile::tempdir;

fn fixture_archived(bytes: &mut Vec<u8>) -> &ArchivedZeroCopyGraph {
    let mut pool = StringPool::new();
    let f_name = pool.add("alpha");
    let c_name = pool.add("Beta");
    let m_name = pool.add("gamma");
    let f_uid = ecp_core::uid::compute(NodeKind::Function, "test.rs", None, "alpha");
    let c_uid = ecp_core::uid::compute(NodeKind::Class, "test.rs", None, "Beta");
    let m_uid = ecp_core::uid::compute(NodeKind::Method, "test.rs", Some("Beta"), "gamma");

    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![],
        nodes: vec![
            Node {
                uid: f_uid,
                name: f_name,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (1, 0, 2, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            },
            Node {
                uid: c_uid,
                name: c_name,
                file_idx: 0,
                kind: NodeKind::Class,
                span: (3, 0, 4, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            },
            Node {
                uid: m_uid,
                name: m_name,
                file_idx: 0,
                kind: NodeKind::Method,
                span: (5, 0, 6, 0),
                community_id: 0,
                owner_class: c_name,
                content_hash: 0,
            },
        ],
        edges: vec![],
        out_offsets: vec![0, 0, 0, 0],
        in_offsets: vec![0, 0, 0, 0],
        in_edge_idx: vec![],
        name_index: Vec::new(),
        process_start: 3,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
    };

    *bytes = rkyv::to_bytes::<Error>(&graph).unwrap().into_vec();
    rkyv::access::<ArchivedZeroCopyGraph, Error>(bytes).unwrap()
}

fn names_returned(cypher_query: &str) -> Vec<String> {
    let mut bytes = Vec::new();
    let archived = fixture_archived(&mut bytes);
    let toks = tokenize(cypher_query).unwrap();
    let q = parse_query(&toks).unwrap();
    let repo = tempdir().unwrap();
    let result = cypher::execute(&q, archived, repo.path()).unwrap();
    result
        .rows
        .iter()
        .map(|r| match &r[0] {
            cypher::Value::Str(s) => s.clone(),
            v => panic!("expected Str, got {v:?}"),
        })
        .collect()
}

#[test]
fn where_label_single_filters_to_kind() {
    let mut names = names_returned("MATCH (n) WHERE n:Function RETURN n.name");
    names.sort();
    assert_eq!(names, vec!["alpha"]);
}

#[test]
fn where_label_pipe_disjunction_matches_either() {
    let mut names = names_returned("MATCH (n) WHERE n:Function|Class RETURN n.name");
    names.sort();
    assert_eq!(names, vec!["Beta", "alpha"]);
}

#[test]
fn where_label_three_way_disjunction_covers_all() {
    let mut names = names_returned("MATCH (n) WHERE n:Function|Class|Method RETURN n.name");
    names.sort();
    assert_eq!(names, vec!["Beta", "alpha", "gamma"]);
}

#[test]
fn where_label_unknown_label_matches_nothing() {
    let names = names_returned("MATCH (n) WHERE n:DoesNotExist RETURN n.name");
    assert!(names.is_empty(), "unknown label must produce zero rows");
}

/// Label that IS a real `NodeKind` variant but no node in the fixture
/// carries it — must still return empty.  Pins the behaviour separately
/// from the bogus-label case in case a future change starts validating
/// label names against the enum at parse time.
#[test]
fn where_label_valid_kind_absent_in_fixture() {
    let names = names_returned("MATCH (n) WHERE n:Trait RETURN n.name");
    assert!(
        names.is_empty(),
        "valid-but-absent label must produce zero rows"
    );
}

#[test]
fn where_label_combined_with_property_predicate() {
    let mut names = names_returned("MATCH (n) WHERE n:Method AND n.name = 'gamma' RETURN n.name");
    names.sort();
    assert_eq!(names, vec!["gamma"]);
}

#[test]
fn where_label_negation_excludes_kind() {
    let mut names = names_returned("MATCH (n) WHERE NOT n:Method RETURN n.name");
    names.sort();
    assert_eq!(names, vec!["Beta", "alpha"]);
}

/// Regression for the original failure: `WHERE n:A OR n:B`. Pre-fix this
/// query produced `parse error at byte 6: expected Return, found Some(Colon)`
/// because parse_primary fell through to bare-Var and left `:` unconsumed.
#[test]
fn where_label_or_disjunction_at_expression_level() {
    let mut names = names_returned("MATCH (n) WHERE n:Function OR n:Class RETURN n.name");
    names.sort();
    assert_eq!(names, vec!["Beta", "alpha"]);
}
