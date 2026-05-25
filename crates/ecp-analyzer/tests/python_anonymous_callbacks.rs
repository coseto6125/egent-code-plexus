//! Calls inside a lambda passed as a call argument must be attached to an
//! `<anonymous>` Function node instead of being dropped by `attach_to_enclosing`
//! (filter (A) callback registration).
//!
//! Repro: `sorted(xs, key=lambda i: score(i))` at module top level produced
//! 0 callers for `score` before this change.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PythonProvider::new().expect("provider");
    p.parse_file(Path::new("test.py"), src.as_bytes())
        .expect("parse")
}

fn anonymous_calls(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous"))
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect()
}

#[test]
fn kwarg_lambda_attaches_call_to_anonymous_node() {
    // `key=lambda i: score(i)` — lambda as keyword argument value.
    let g = parse("xs = [1, 2]\nsorted(xs, key=lambda i: score(i))\n");
    assert!(
        anonymous_calls(&g).contains(&"score"),
        "expected score attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn positional_lambda_attaches_call_to_anonymous_node() {
    // Lambda as positional argument — `filter(lambda x: validate(x), xs)`.
    let g = parse("xs = [1, 2]\nfilter(lambda x: validate(x), xs)\n");
    assert!(
        anonymous_calls(&g).contains(&"validate"),
        "expected validate attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_lambda_emits_no_anonymous_node() {
    // No call in the lambda body — no <anonymous> node should appear.
    let g = parse("xs = [1, 2]\nsorted(xs, key=lambda i: i)\n");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "lambda without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn named_function_arg_is_not_treated_as_anonymous_callback() {
    // Passing a named function by reference (`key=scorer`) is not a lambda —
    // no <anonymous> node, and no spurious call edge.
    let g = parse("def scorer(x):\n    return x\nxs = [1]\nsorted(xs, key=scorer)\n");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "named-fn argument must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
