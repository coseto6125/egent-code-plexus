//! Calls inside an anonymous closure attached to a call must be attributed to
//! an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` (filter (A) callback registration).
//!
//! Repro shape: `fetchData { r in handle(r) }` at module top level — no named
//! enclosing scope, so without an `<anonymous>` node `handle` has 0 callers.
//! Swift-specific: closures appear both as trailing blocks (outside arg parens)
//! and as value_argument expressions (inside parens).

use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = SwiftProvider::new().expect("provider");
    p.parse_file(Path::new("test.swift"), src.as_bytes())
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
fn trailing_closure_attaches_call_to_anonymous_node() {
    // `fetchData { r in handle(r) }` — trailing closure. Multi-line because
    // attach_to_enclosing is row-granular: the inner call must sit on a row the
    // closure spans more narrowly than the enclosing func.
    let g = parse("func m() {\n    fetchData { r in\n        handle(r)\n    }\n}");
    assert!(
        anonymous_calls(&g).contains(&"handle"),
        "expected handle attached to <anonymous> (trailing closure), nodes: {:?}",
        g.nodes
    );
}

#[test]
fn arg_position_closure_attaches_call_to_anonymous_node() {
    // `items.map({ x in f(x) })` — closure inside value_arguments.
    let g = parse("func m() {\n    items.map({ x in\n        f(x)\n    })\n}");
    assert!(
        anonymous_calls(&g).contains(&"f"),
        "expected f attached to <anonymous> (arg-position closure), nodes: {:?}",
        g.nodes
    );
}

#[test]
fn dispatch_queue_trailing_closure_attaches_call() {
    // `DispatchQueue.main.async { update() }` — common Swift pattern.
    let g = parse("func m() {\n    DispatchQueue.main.async {\n        update()\n    }\n}");
    assert!(
        anonymous_calls(&g).contains(&"update"),
        "expected update attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_closure_emits_no_anonymous_node() {
    // `items.map { $0 + 1 }` — no call inside, must not add graph bloat.
    let g = parse("func m() { items.map { $0 + 1 } }");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "closure without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn named_function_arg_is_not_treated_as_anonymous_callback() {
    // Passing a named function by reference is a reference, not a call site.
    let g = parse("func m() { btn.addTarget(self, action: closeIt) }");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "named-fn argument must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
