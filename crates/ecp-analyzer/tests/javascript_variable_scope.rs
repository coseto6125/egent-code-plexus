//! Regression: JS `@variable.name` captures were unscoped, so locals
//! inside function bodies were emitted as Variable. Round 8 baseline
//! showed rs=1815 vs ref=767 — +1048 over-emit (~2.4x). The fix anchors
//! `lexical_declaration` / `variable_declaration` to direct children of
//! `program`. Function-body locals are excluded.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = JavaScriptProvider::new().expect("provider");
    p.parse_file(Path::new("test.js"), src.as_bytes())
        .expect("parse")
}

fn variables(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variable)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn module_level_let_emits_variable() {
    let g = parse("let x = 1;");
    assert_eq!(variables(&g), vec!["x"], "nodes: {:?}", g.nodes);
}

#[test]
fn module_level_var_emits_variable() {
    let g = parse("var x = 1;");
    assert_eq!(variables(&g), vec!["x"], "nodes: {:?}", g.nodes);
}

#[test]
fn function_body_local_let_does_not_emit() {
    let g = parse("function f() { let y = 2; }");
    let vars = variables(&g);
    assert!(
        !vars.contains(&"y"),
        "function-body local should not emit Variable: {:?}",
        g.nodes
    );
}

#[test]
fn function_body_local_var_does_not_emit() {
    let g = parse("function f() { var y = 2; }");
    let vars = variables(&g);
    assert!(
        !vars.contains(&"y"),
        "function-body var should not emit Variable: {:?}",
        g.nodes
    );
}

#[test]
fn nested_block_let_does_not_emit() {
    let g = parse("if (cond) { let inner = 3; }");
    let vars = variables(&g);
    assert!(
        !vars.contains(&"inner"),
        "block-scope let should not emit Variable: {:?}",
        g.nodes
    );
}

#[test]
fn module_level_with_function_body_only_top_emits() {
    let g = parse("let outer = 1; function f() { let inner = 2; }");
    let vars = variables(&g);
    assert_eq!(vars, vec!["outer"], "nodes: {:?}", g.nodes);
}

#[test]
fn export_let_still_emits() {
    let g = parse("export let exported = 5;");
    let vars = variables(&g);
    assert_eq!(
        vars,
        vec!["exported"],
        "exported top-level should still emit"
    );
}
