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
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use tempfile::tempdir;

fn fixture_archived(bytes: &mut Vec<u8>) -> &ArchivedZeroCopyGraph {
    let mut fx = GraphFixture::new();
    let f = fx.func("test.rs", "alpha");
    fx.span(f, (1, 0, 2, 0));
    let c = fx.node(NodeKind::Class, "test.rs", "Beta");
    fx.span(c, (3, 0, 4, 0));
    let m = fx.method("test.rs", "Beta", "gamma");
    fx.span(m, (5, 0, 6, 0));

    *bytes = fx.into_bytes();
    rkyv::access::<ArchivedZeroCopyGraph, Error>(bytes).unwrap()
}

fn names_returned(cypher_query: &str) -> Vec<String> {
    let mut bytes = Vec::new();
    let archived = fixture_archived(&mut bytes);
    let toks = tokenize(cypher_query).unwrap();
    let q = parse_query(&toks).unwrap();
    let repo = tempdir().unwrap();
    let result = cypher::execute(&q, archived, None, repo.path()).unwrap();
    result
        .rows
        .iter()
        .map(|r| match &r[0] {
            cypher::Value::Str(s) => s.to_string(),
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
