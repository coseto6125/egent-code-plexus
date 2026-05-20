use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = GoProvider::new().expect("provider");
    p.parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse")
}

#[test]
fn var_single_emits_variable_kind() {
    let g = parse("package p\n\nvar X int = 1\n");
    let x = g.nodes.iter().find(|n| n.name == "X").expect("X missing");
    assert_eq!(x.kind, NodeKind::Variable, "got {:?}", x);
}

#[test]
fn var_block_emits_each_variable() {
    let g = parse("package p\n\nvar (\n    A int = 1\n    B string = \"x\"\n)\n");
    let a = g.nodes.iter().find(|n| n.name == "A").expect("A missing");
    let b = g.nodes.iter().find(|n| n.name == "B").expect("B missing");
    assert_eq!(a.kind, NodeKind::Variable);
    assert_eq!(b.kind, NodeKind::Variable);
}

#[test]
fn var_inferred_type_emits_variable_kind() {
    let g = parse("package p\n\nvar X = 42\n");
    let x = g.nodes.iter().find(|n| n.name == "X").expect("X missing");
    assert_eq!(x.kind, NodeKind::Variable, "got {:?}", x);
}

#[test]
fn short_var_decl_emits_variable_no_type() {
    let g = parse("package p\nfunc f() { x := 1; _ = x }\n");
    let x = g
        .nodes
        .iter()
        .find(|n| n.name == "x" && n.kind == NodeKind::Variable)
        .expect("short-decl x must emit Variable");
    assert!(
        x.type_annotation.is_none(),
        "short-decl must have no type_annotation"
    );
}

#[test]
fn short_var_decl_multi_name() {
    let g = parse("package p\nfunc f() { a, b := 1, 2; _ = a; _ = b }\n");
    let a = g
        .nodes
        .iter()
        .find(|n| n.name == "a" && n.kind == NodeKind::Variable)
        .expect("short-decl a must emit Variable");
    let b = g
        .nodes
        .iter()
        .find(|n| n.name == "b" && n.kind == NodeKind::Variable)
        .expect("short-decl b must emit Variable");
    assert!(a.type_annotation.is_none());
    assert!(b.type_annotation.is_none());
}

#[test]
fn short_var_decl_blank_identifier_skipped() {
    let g = parse("package p\nfunc f() { _, err := someFunc(); _ = err }\n");
    assert!(
        g.nodes.iter().all(|n| n.name != "_"),
        "_ must not emit Variable"
    );
    // err should be emitted
    assert!(g
        .nodes
        .iter()
        .any(|n| n.name == "err" && n.kind == NodeKind::Variable));
}

#[test]
fn short_var_decl_complex_rhs() {
    // Matches tree.go patterns: `cs := n.children`, `fullPath := path`
    let src = "package p\nfunc f() {\n    cs := n.children\n    fullPath := path\n    escapeColon := false\n}\n";
    let g = parse(src);
    let vars: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variable)
        .map(|n| n.name.as_str())
        .collect();
    assert!(vars.contains(&"cs"), "cs missing: {vars:?}");
    assert!(vars.contains(&"fullPath"), "fullPath missing: {vars:?}");
    assert!(
        vars.contains(&"escapeColon"),
        "escapeColon missing: {vars:?}"
    );
    // No single-char fragments from identifiers
    assert!(!vars.contains(&"n"), "n should not be captured: {vars:?}");
}
