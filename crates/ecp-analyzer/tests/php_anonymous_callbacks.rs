//! Calls inside an anonymous callback passed as a call argument must be
//! attached to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` (filter (A) callback registration).
//!
//! Repro: `array_map(function($x) { return transform($x); }, $a)` at module
//! top level produced 0 callers for `transform` before this change.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("provider");
    p.parse_file(Path::new("test.php"), src.as_bytes())
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
fn top_level_anon_function_callback_attaches_call_to_anonymous_node() {
    let g = parse(r#"<?php array_map(function($x) { return transform($x); }, $a); ?>"#);
    assert!(
        anonymous_calls(&g).contains(&"transform"),
        "expected transform attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn top_level_arrow_fn_callback_attaches_call_to_anonymous_node() {
    let g = parse(r#"<?php array_map(fn($x) => process($x), $a); ?>"#);
    assert!(
        anonymous_calls(&g).contains(&"process"),
        "expected process attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_callback_emits_no_anonymous_node() {
    // No call inside — <anonymous> node must not be emitted (no graph bloat).
    let g = parse(r#"<?php array_map(fn($x) => $x * 2, $a); ?>"#);
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "callback without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn named_function_string_arg_is_not_treated_as_anonymous_callback() {
    // Passing a function name as a string is not a callable literal in the
    // tree — it's just a string argument; no <anonymous> node.
    let g = parse(r#"<?php array_map('strtolower', $a); ?>"#);
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "string-callable argument must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
