//! Regression: TypeScript `@const.name` / `@variable.name` captures were
//! unscoped — every `const x = …` inside a function body / test case /
//! block emitted a Const node. Round 8 baseline showed Const rs=7309 vs
//! ref=4832 — +2477 over-emit. The fix anchors both `lexical_declaration`
//! and `variable_declaration` patterns to direct children of `program`.

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

fn consts(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .map(|n| n.name.as_str())
        .collect()
}

fn vars(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variable)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn module_level_const_emits() {
    let g = parse("const X = 1;");
    assert_eq!(consts(&g), vec!["X"], "nodes: {:?}", g.nodes);
}

#[test]
fn module_level_var_emits() {
    let g = parse("var y = 1;");
    assert_eq!(vars(&g), vec!["y"], "nodes: {:?}", g.nodes);
}

#[test]
fn function_body_const_does_not_emit() {
    let g = parse("function f() { const local = 2; }");
    let cs = consts(&g);
    assert!(
        !cs.contains(&"local"),
        "function-body const must not emit: {:?}",
        g.nodes
    );
}

#[test]
fn function_body_let_does_not_emit() {
    let g = parse("function f() { let local = 2; }");
    let cs = consts(&g);
    assert!(
        !cs.contains(&"local"),
        "function-body let must not emit Const: {:?}",
        g.nodes
    );
}

#[test]
fn function_body_var_does_not_emit() {
    let g = parse("function f() { var local = 2; }");
    let v = vars(&g);
    assert!(
        !v.contains(&"local"),
        "function-body var must not emit Variable: {:?}",
        g.nodes
    );
}

#[test]
fn block_scope_const_does_not_emit() {
    let g = parse("if (cond) { const inner = 3; }");
    let cs = consts(&g);
    assert!(
        !cs.contains(&"inner"),
        "block-scope const must not emit: {:?}",
        g.nodes
    );
}

#[test]
fn export_const_still_emits() {
    let g = parse("export const Exported = 5;");
    let cs = consts(&g);
    assert_eq!(cs, vec!["Exported"], "exported top-level should still emit");
}

#[test]
fn module_level_only_emits_top_const() {
    let g = parse("const outer = 1; function f() { const inner = 2; }");
    let cs = consts(&g);
    assert_eq!(cs, vec!["outer"], "nodes: {:?}", g.nodes);
}
