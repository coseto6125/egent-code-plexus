//! Calls inside an anonymous lambda attached to a call (trailing or
//! paren-position) must be attached to an `<anonymous>` Function node instead
//! of being dropped by `attach_to_enclosing` (filter (A) callback registration).
//!
//! Kotlin lambdas are usually TRAILING, outside the argument parens:
//! `list.forEach { process(it) }` → `call_suffix → annotated_lambda →
//! lambda_literal`.

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = KotlinProvider::new().expect("provider");
    p.parse_file(Path::new("test.kt"), src.as_bytes())
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
fn trailing_lambda_attaches_call_to_anonymous_node() {
    let g = parse("fun m(list: List<Int>) {\n    list.forEach {\n        process(it)\n    }\n}");
    assert!(
        anonymous_calls(&g).contains(&"process"),
        "expected process attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn top_level_trailing_lambda_attaches_call() {
    // Lambda registered at file scope (the original drop site).
    let g = parse("val cb = register {\n    handle()\n}");
    assert!(
        anonymous_calls(&g).contains(&"handle"),
        "expected handle attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_lambda_emits_no_anonymous_node() {
    let g = parse("fun m(list: List<Int>) {\n    list.map { it + 1 }\n}");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "lambda without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn function_reference_is_not_treated_as_anonymous_callback() {
    // `::process` is a callable reference, not a lambda_literal — no node.
    let g = parse("fun m(list: List<Int>) {\n    list.forEach(::process)\n}");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "function reference must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
