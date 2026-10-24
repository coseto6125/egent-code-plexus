//! Conformance tests: each construct either evaluates with openCypher semantics
//! or is refused with an error that names the construct. A confident wrong
//! answer is the failure mode these pin down, so the refusals assert the message.
//!
//! Graph (all `Calls` edges):
//!   alpha -> beta, alpha -> gamma, beta -> gamma, dup(x.rs) -> gamma, dup(y.rs) -> gamma
//! plus two unconnected functions `item7` and `itemd`.

use ecp_core::cypher::{self, QueryResult, Value};
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType};
use ecp_core::graph_fixture::GraphFixture;
use std::path::Path;

fn graph_bytes() -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let alpha = fx.func("src/a.rs", "alpha");
    let beta = fx.func("src/a.rs", "beta");
    let gamma = fx.func("src/a.rs", "gamma");
    let dup_x = fx.func("src/x.rs", "dup");
    let dup_y = fx.func("src/y.rs", "dup");
    fx.func("src/z.rs", "item7");
    fx.func("src/z.rs", "itemd");
    fx.edge(alpha, beta, RelType::Calls);
    fx.edge(alpha, gamma, RelType::Calls);
    fx.edge(beta, gamma, RelType::Calls);
    fx.edge(dup_x, gamma, RelType::Calls);
    fx.edge(dup_y, gamma, RelType::Calls);
    fx.into_bytes()
}

fn run(text: &str) -> Result<QueryResult, cypher::CypherError> {
    let bytes = graph_bytes();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    cypher::execute(&cypher::parse(text)?, graph, None, Path::new("."))
}

fn rows(text: &str) -> Vec<Vec<Value>> {
    run(text)
        .unwrap_or_else(|error| panic!("{text}: {error}"))
        .rows
}

fn text(s: &str) -> Value {
    Value::Str(s.into())
}

fn parse_error(text: &str) -> String {
    cypher::parse(text)
        .expect_err("query must be refused at parse time")
        .to_string()
}

// ---- comma patterns are a join -------------------------------------------

#[test]
fn test_execute_comma_patterns_materialising_return_one_joined_row() {
    // Old: a union, [[alpha, null], [null, beta]].
    assert_eq!(
        rows("MATCH (a {name:'alpha'}), (b {name:'beta'}) RETURN a.name, b.name"),
        vec![vec![text("alpha"), text("beta")]]
    );
}

#[test]
fn test_execute_comma_patterns_streaming_multiply_unrelated_patterns() {
    // 1 alpha x 2 dup = 2 rows (old union: 1 + 2 = 3).
    assert_eq!(
        rows("MATCH (a:Function {name:'alpha'}), (b:Function {name:'dup'}) RETURN count(*)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_execute_comma_patterns_join_on_shared_variable() {
    // alpha->beta->gamma is the only chain (old union: 10 rows).
    assert_eq!(
        rows("MATCH (a)-[:Calls]->(b), (b)-[:Calls]->(c) RETURN count(*)"),
        vec![vec![Value::Int(1)]]
    );
    assert_eq!(
        rows("MATCH (a)-[:Calls]->(b), (b)-[:Calls]->(c) RETURN a.name, b.name, c.name"),
        vec![vec![text("alpha"), text("beta"), text("gamma")]]
    );
}

#[test]
fn test_execute_optional_match_with_comma_patterns_is_one_optional_unit() {
    // The clause as a whole fails (`nope` does not exist), so `z` keeps one row
    // with both optional variables unset. Old: a second row with b = beta.
    assert_eq!(
        rows(
            "MATCH (z {name:'alpha'}) OPTIONAL MATCH (a {name:'nope'}), (b {name:'beta'}) \
             RETURN z.name, a.name, b.name"
        ),
        vec![vec![text("alpha"), Value::Null, Value::Null]]
    );
}

#[test]
fn test_execute_single_pattern_match_rows_unchanged() {
    assert_eq!(
        rows("MATCH (a)-[:Calls]->(b) RETURN count(*)"),
        vec![vec![Value::Int(5)]]
    );
}

// ---- EXISTS with both endpoints bound ------------------------------------

#[test]
fn test_execute_exists_with_both_endpoints_bound_checks_the_pair() {
    // alpha calls beta and gamma only. Old: every function counted (7).
    assert_eq!(
        rows(
            "MATCH (a {name:'alpha'}) MATCH (b:Function) WHERE EXISTS((a)-[:Calls]->(b)) RETURN count(b)"
        ),
        vec![vec![Value::Int(2)]]
    );
    // 7 functions - 2 callees. Old: NOT of an always-true test, 0.
    assert_eq!(
        rows(
            "MATCH (a {name:'alpha'}) MATCH (b:Function) WHERE NOT EXISTS((a)-[:Calls]->(b)) RETURN count(b)"
        ),
        vec![vec![Value::Int(5)]]
    );
}

#[test]
fn test_execute_exists_with_reversed_pair_finds_no_caller_of_alpha() {
    // Nothing calls alpha. Old: anchored on b, true for every node with a callee (4).
    assert_eq!(
        rows(
            "MATCH (a {name:'alpha'}) MATCH (b:Function) WHERE EXISTS((b)-[:Calls]->(a)) RETURN count(b)"
        ),
        vec![vec![Value::Int(0)]]
    );
}

#[test]
fn test_execute_exists_anchor_must_match_its_own_label() {
    // `alpha` is a Function, not a Class. Old: the anchor's label was not checked.
    assert_eq!(
        rows("MATCH (a {name:'alpha'}) WHERE EXISTS((a:Class)-[:Calls]->()) RETURN count(a)"),
        vec![vec![Value::Int(0)]]
    );
}

// ---- WITH / WHERE clause order -------------------------------------------

#[test]
fn test_parse_second_with_is_refused_naming_the_clause() {
    let error =
        parse_error("MATCH (a:Function) WITH a WHERE a.name = 'alpha' WITH a RETURN count(a)");
    assert!(error.contains("a second WITH clause"), "{error}");
}

#[test]
fn test_parse_match_after_with_is_refused_naming_the_clause() {
    let error = parse_error("MATCH (a:Function) WITH a MATCH (b) RETURN count(b)");
    assert!(error.contains("MATCH after WITH"), "{error}");
}

#[test]
fn test_parse_where_after_with_body_is_refused() {
    let error = parse_error(
        "MATCH (a:Function) WITH a WHERE a.name = 'alpha' WHERE a.name = 'beta' RETURN a.name",
    );
    assert!(error.contains("WHERE after WITH"), "{error}");
}

#[test]
fn test_execute_where_per_match_clause_is_anded() {
    // Old: the second WHERE overwrote the first, giving one row per function.
    assert_eq!(
        rows(
            "MATCH (a:Function) WHERE a.name = 'alpha' MATCH (b:Function) WHERE b.name = 'beta' \
             RETURN a.name, b.name"
        ),
        vec![vec![text("alpha"), text("beta")]]
    );
}

#[test]
fn test_parse_second_where_with_optional_match_is_refused() {
    let error = parse_error(
        "MATCH (a:Function) WHERE a.name = 'alpha' OPTIONAL MATCH (a)-[:Calls]->(b) \
         WHERE b.name = 'beta' RETURN a.name",
    );
    assert!(
        error.contains("WHERE directly after OPTIONAL MATCH"),
        "{error}"
    );
}

// ---- DISTINCT on a node variable -----------------------------------------

#[test]
fn test_execute_count_distinct_node_variable_counts_nodes_not_names() {
    // Two distinct nodes share the name `dup`. Old: 1.
    assert_eq!(
        rows("MATCH (f {name:'dup'}) RETURN count(DISTINCT f)"),
        vec![vec![Value::Int(2)]]
    );
    // Control: the name property is one value.
    assert_eq!(
        rows("MATCH (f {name:'dup'}) RETURN count(DISTINCT f.name)"),
        vec![vec![Value::Int(1)]]
    );
}

#[test]
fn test_execute_collect_distinct_node_variable_keeps_one_entry_per_node() {
    assert_eq!(
        rows("MATCH (f {name:'dup'}) RETURN collect(DISTINCT f)"),
        vec![vec![Value::List(vec![text("dup"), text("dup")])]]
    );
}

#[test]
fn test_execute_count_distinct_node_variable_over_empty_match_is_zero() {
    assert_eq!(
        rows("MATCH (f {name:'nope'}) RETURN count(DISTINCT f)"),
        vec![vec![Value::Int(0)]]
    );
}

// ---- regex escapes --------------------------------------------------------

#[test]
fn test_execute_regex_digit_class_reaches_the_regex_engine() {
    // Old: `\d` became the letter `d`, so `itemd` matched and `item7` did not.
    for pattern in [r"item\d", r"item\\d"] {
        assert_eq!(
            rows(&format!(
                "MATCH (n:Function) WHERE n.name =~ '{pattern}' RETURN n.name"
            )),
            vec![vec![text("item7")]],
            "{pattern}"
        );
    }
}

#[test]
fn test_execute_whitespace_only_literal_matches_no_name() {
    assert_eq!(
        rows("MATCH (n:Function) WHERE n.name = '   ' RETURN count(n)"),
        vec![vec![Value::Int(0)]]
    );
}

// ---- ORDER BY on a key outside the projection ------------------------------

#[test]
fn test_execute_order_by_key_not_in_projection_is_refused() {
    let error = run("MATCH (a:Function) RETURN a.name ORDER BY a.startLine")
        .expect_err("unprojected sort key must be refused")
        .to_string();
    assert!(error.contains("ORDER BY 'a.startLine'"), "{error}");
}

#[test]
fn test_execute_order_by_projected_alias_and_property_sort() {
    let sorted = rows("MATCH (a:Function) RETURN a.name AS n ORDER BY n DESC");
    assert_eq!(sorted[0], vec![text("itemd")]);
    assert_eq!(sorted.last().unwrap(), &vec![text("alpha")]);
}

#[test]
fn test_execute_order_by_bare_node_variable_sorts_by_name() {
    // Old: the key was ignored and rows stayed in node order (alpha first).
    let sorted = rows("MATCH (a:Function) RETURN a ORDER BY a DESC");
    assert_eq!(sorted[0][0], text("itemd"));
}

#[test]
fn test_execute_order_by_over_empty_result_is_not_an_error() {
    // An empty result keeps `a` as one unexpanded column; `a.name` has no
    // column there, but there is nothing to sort.
    assert!(rows("MATCH (a:Class) RETURN a ORDER BY a.name").is_empty());
}

// ---- unknown function -------------------------------------------------------

#[test]
fn test_parse_unknown_function_names_the_function() {
    for query in [
        "MATCH (n) RETURN toLower(n.name)",
        "MATCH (n) WHERE size(n.name) > 1 RETURN n.name",
        "MATCH (n) RETURN count(frobnicate(n))",
    ] {
        let error = parse_error(query);
        assert!(error.contains("unknown function"), "{query}: {error}");
    }
    assert!(parse_error("MATCH (n) RETURN toLower(n.name)").contains("'toLower'"));
}

#[test]
fn test_execute_known_functions_still_evaluate() {
    assert_eq!(
        rows("MATCH (n {name:'alpha'}) RETURN labels(n)"),
        vec![vec![Value::List(vec![text("Function")])]]
    );
    assert_eq!(
        rows("MATCH (a)-[r:Calls]->(b) RETURN DISTINCT type(r)"),
        vec![vec![text("Calls")]]
    );
}

// ---- variable-length patterns ------------------------------------------------

#[test]
fn test_parse_variable_length_without_distinct_is_refused() {
    for query in [
        "MATCH (a {name:'alpha'})-[:Calls*1..2]->(b) RETURN b.name",
        "MATCH (a {name:'alpha'})-[:Calls*1..2]->(b) RETURN count(*)",
        "MATCH (a {name:'alpha'})-[:Calls*1..2]->(b) RETURN count(b)",
        "MATCH (a {name:'alpha'})-[:Calls*1..2]->(b) WITH b RETURN b.name",
    ] {
        let error = parse_error(query);
        assert!(
            error.contains("variable-length pattern") && error.contains("DISTINCT"),
            "{query}: {error}"
        );
    }
}

#[test]
fn test_parse_variable_length_edge_variable_is_refused() {
    let error = parse_error("MATCH (a)-[r:Calls*1..2]->(b) RETURN DISTINCT b.name");
    assert!(error.contains("relationship variable 'r'"), "{error}");
}

#[test]
fn test_parse_variable_length_minimum_above_one_is_refused() {
    // A two-node cycle cx <-> cy: the walk cx->cy->cx->cy reaches cy in 3 hops,
    // but a trail cannot repeat cx->cy, so `*3..3` must not answer from walks.
    // Old: accepted, and returned cy.
    for query in [
        "MATCH (a)-[:Calls*2..]->(b) RETURN DISTINCT b.name",
        "MATCH (a)-[:Calls*2..2]->(b) RETURN DISTINCT b.name",
        "MATCH (a {name:'cx'})-[:Calls*3..3]->(t) RETURN DISTINCT t.name",
        "MATCH (a) WHERE EXISTS((a)-[:Calls*2..3]->()) RETURN count(a)",
        "MATCH (a) WHERE EXISTS((a)-[:Calls*2..]->()) RETURN count(a)",
    ] {
        let error = parse_error(query);
        assert!(error.contains("minimum above 1"), "{query}: {error}");
    }
}

#[test]
fn test_parse_where_directly_after_optional_match_is_refused() {
    // openCypher scopes this WHERE to the optional pattern (every `f` would
    // keep a row, with c null); this engine filtered all rows afterwards, so it
    // answered "uncalled functions" instead. Old: parsed and ran.
    let error = parse_error(
        "MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) WHERE c IS NULL RETURN f.name",
    );
    assert!(
        error.contains("WHERE directly after OPTIONAL MATCH"),
        "{error}"
    );
}

#[test]
fn test_execute_optional_match_then_with_where_filters_rows() {
    // The spelling the refusal points to: rows without a caller.
    assert_eq!(
        rows(
            "MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) WITH f, c WHERE c IS NULL \
             RETURN f.name ORDER BY f.name"
        )
        .len(),
        5 // alpha, dup, dup, item7, itemd have no caller; beta and gamma do
    );
}

#[test]
fn test_execute_variable_length_set_valued_shapes_return_reachability() {
    assert_eq!(
        rows("MATCH (a {name:'alpha'})-[:Calls*1..2]->(b) RETURN DISTINCT b.name"),
        vec![vec![text("beta")], vec![text("gamma")]]
    );
    assert_eq!(
        rows("MATCH (a {name:'alpha'})-[:Calls*1..2]->(b) RETURN count(DISTINCT b)"),
        vec![vec![Value::Int(2)]]
    );
    assert_eq!(
        rows("MATCH (a {name:'alpha'})-[:Calls*1..2]->(b) RETURN min(b.name), max(b.name)"),
        vec![vec![text("beta"), text("gamma")]]
    );
    // A fixed single hop is not variable-length and keeps its edge variable.
    assert_eq!(
        rows("MATCH (a {name:'alpha'})-[r:Calls*1..1]->(b) RETURN count(r)"),
        vec![vec![Value::Int(2)]]
    );
}

// ---- cycles back to the start node ------------------------------------------

/// cx <-> cy (two Calls edges), `loop` -> `loop`, and a triangle t1-t2-t3.
fn cycle_rows(text: &str) -> Vec<String> {
    let mut fx = GraphFixture::new();
    let cx = fx.func("src/c.rs", "cx");
    let cy = fx.func("src/c.rs", "cy");
    let lp = fx.func("src/c.rs", "loop");
    let t1 = fx.func("src/c.rs", "t1");
    let t2 = fx.func("src/c.rs", "t2");
    let t3 = fx.func("src/c.rs", "t3");
    fx.edge(cx, cy, RelType::Calls);
    fx.edge(cy, cx, RelType::Calls);
    fx.edge(lp, lp, RelType::Calls);
    fx.edge(t1, t2, RelType::Calls);
    fx.edge(t2, t3, RelType::Calls);
    fx.edge(t3, t1, RelType::Calls);
    let bytes = fx.into_bytes();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let result = cypher::execute(&cypher::parse(text).unwrap(), graph, None, Path::new("."))
        .unwrap_or_else(|error| panic!("{text}: {error}"));
    let mut names: Vec<String> = result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::Str(s) => s.to_string(),
            other => panic!("expected a name, got {other:?}"),
        })
        .collect();
    names.sort();
    names
}

#[test]
fn test_execute_variable_length_returns_start_when_a_trail_closes_the_cycle() {
    // cx->cy->cx is a closed trail of 2 hops. Old: start was never returned.
    assert_eq!(
        cycle_rows("MATCH (a {name:'cx'})-[:Calls*1..2]->(t) RETURN DISTINCT t.name"),
        vec!["cx", "cy"]
    );
    // One hop is not enough to close it.
    assert_eq!(
        cycle_rows("MATCH (a {name:'cx'})-[:Calls*1..1]->(t) RETURN DISTINCT t.name"),
        vec!["cy"]
    );
}

#[test]
fn test_execute_variable_length_returns_start_for_a_self_loop() {
    assert_eq!(
        cycle_rows("MATCH (a {name:'loop'})-[:Calls*1..3]->(t) RETURN DISTINCT t.name"),
        vec!["loop"]
    );
}

#[test]
fn test_execute_variable_length_acyclic_start_is_not_returned() {
    // The triangle closes in 3 hops; a 2-hop budget must not return t1.
    assert_eq!(
        cycle_rows("MATCH (a {name:'t1'})-[:Calls*1..2]->(t) RETURN DISTINCT t.name"),
        vec!["t2", "t3"]
    );
    assert_eq!(
        cycle_rows("MATCH (a {name:'t1'})-[:Calls*1..3]->(t) RETURN DISTINCT t.name"),
        vec!["t1", "t2", "t3"]
    );
}

#[test]
fn test_execute_variable_length_undirected_does_not_reuse_a_relationship() {
    // Undirected, a walk may step back over the edge it just used; a trail
    // may not. The triangle has one edge per pair, so t1 is not reachable
    // from t1 in 2 hops (t1-t2-t1 reuses an edge) but is in 3.
    assert_eq!(
        cycle_rows("MATCH (a {name:'t1'})-[:Calls*1..2]-(t) RETURN DISTINCT t.name"),
        vec!["t2", "t3"]
    );
    assert_eq!(
        cycle_rows("MATCH (a {name:'t1'})-[:Calls*1..3]-(t) RETURN DISTINCT t.name"),
        vec!["t1", "t2", "t3"]
    );
}

#[test]
fn test_execute_exists_variable_length_sees_the_cycle_back_to_start() {
    assert_eq!(
        cycle_rows("MATCH (a:Function) WHERE EXISTS((a)-[:Calls*1..2]->(a)) RETURN a.name"),
        vec!["cx", "cy", "loop"]
    );
}
