//! Calls inside an anonymous callback passed as a call argument must be
//! attached to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` (filter (A) callback registration).
//!
//! Repro: `el.addEventListener('click', () => guardedClose(...))` at module
//! top level produced 0 callers for `guardedClose` before this change.

use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = TypeScriptProvider::new().expect("provider");
    p.parse_file(Path::new("test.ts"), src.as_bytes())
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
fn top_level_arrow_callback_attaches_call_to_anonymous_node() {
    let g = parse(
        "document.getElementById('x').addEventListener('click', () => guardedClose('m', closeIt));",
    );
    assert!(
        anonymous_calls(&g).contains(&"guardedClose"),
        "expected guardedClose attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn top_level_function_expression_callback_attaches_call() {
    let g = parse("setTimeout(function () { doWork(); }, 100);");
    assert!(
        anonymous_calls(&g).contains(&"doWork"),
        "expected doWork attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_callback_emits_no_anonymous_node() {
    let g = parse("arr.map(x => x * 2);");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "callback without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn named_function_arg_is_not_treated_as_anonymous_callback() {
    // Passing a named function by reference is a reference, not a call site —
    // no <anonymous> node, and no spurious call edge.
    let g = parse("btn.addEventListener('click', closeIt);");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "named-fn argument must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
