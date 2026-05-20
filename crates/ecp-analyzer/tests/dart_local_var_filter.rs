//! Verifies that NodeKind::Variable is only emitted for top-level /
//! library-level declarations. Function-local vars, parameters, for-in
//! bindings, and lambda params must NOT produce Variable nodes.

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = DartProvider::new().expect("provider");
    p.parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse")
}

fn has_variable(graph: &LocalGraph, name: &str) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| n.name == name && n.kind == NodeKind::Variable)
}

// ── Top-level should be emitted ───────────────────────────────────────────────

#[test]
fn top_level_typed_var_emits_variable() {
    let g = parse("double pi = 3.14;");
    assert!(
        has_variable(&g, "pi"),
        "`pi` at top level must be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn top_level_var_keyword_emits_variable() {
    let g = parse("var greeting = 'hello';");
    assert!(
        has_variable(&g, "greeting"),
        "`greeting` at top level must be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn top_level_final_emits_variable() {
    let g = parse("final String appName = 'MyApp';");
    assert!(
        has_variable(&g, "appName"),
        "`appName` at top level must be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

// ── Function-locals must NOT be emitted ──────────────────────────────────────

#[test]
fn local_var_inside_function_not_emitted() {
    let g = parse("void foo() { var x = 42; }");
    assert!(
        !has_variable(&g, "x"),
        "local `x` inside function must not be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn local_final_inside_function_not_emitted() {
    let g = parse("void foo() { final result = compute(); }");
    assert!(
        !has_variable(&g, "result"),
        "local `result` inside function must not be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn local_typed_var_inside_function_not_emitted() {
    let g = parse("void foo() { int count = 0; }");
    assert!(
        !has_variable(&g, "count"),
        "local typed `count` inside function must not be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn local_var_inside_method_not_emitted() {
    let g = parse("class C { void m() { var temp = 1; } }");
    assert!(
        !has_variable(&g, "temp"),
        "local `temp` inside method must not be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

// ── For-in locals must NOT be emitted ────────────────────────────────────────

#[test]
fn for_in_binding_not_emitted() {
    let g = parse("void foo() { for (var item in list) { print(item); } }");
    assert!(
        !has_variable(&g, "item"),
        "for-in binding `item` must not be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

// ── Parameters must NOT be emitted ───────────────────────────────────────────

#[test]
fn function_param_not_emitted_as_variable() {
    let g = parse("void foo(int count) {}");
    assert!(
        !has_variable(&g, "count"),
        "parameter `count` must not be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn method_param_not_emitted_as_variable() {
    let g = parse("class C { void m(String name) {} }");
    assert!(
        !has_variable(&g, "name"),
        "method parameter `name` must not be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}

// ── Typedef must not double-emit as Variable ──────────────────────────────────

#[test]
fn typedef_not_emitted_as_variable() {
    let g = parse("typedef _OnHandler = Future<void> Function(int);");
    assert!(
        !has_variable(&g, "_OnHandler"),
        "typedef `_OnHandler` must not be NodeKind::Variable; nodes: {:#?}",
        g.nodes
    );
}
