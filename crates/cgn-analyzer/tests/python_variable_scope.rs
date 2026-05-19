//! Regression: Python `@variable.name` captured any `x = ...` assignment
//! at any depth. Round 8 baseline showed rs=1215 vs ref=695 (+520 over).
//! Fix anchors `expression_statement > assignment` to `module` direct
//! children. Function- and class-body assignments are excluded.

use cgn_analyzer::python::parser::PythonProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PythonProvider::new().expect("provider");
    p.parse_file(Path::new("test.py"), src.as_bytes())
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
fn module_level_assignment_emits_variable() {
    let g = parse("x = 1\n");
    assert_eq!(variables(&g), vec!["x"], "nodes: {:?}", g.nodes);
}

#[test]
fn module_level_annotated_emits_variable() {
    let g = parse("x: int = 1\n");
    assert_eq!(variables(&g), vec!["x"], "nodes: {:?}", g.nodes);
}

#[test]
fn function_body_local_does_not_emit() {
    let g = parse("def f():\n    y = 2\n");
    let vars = variables(&g);
    assert!(
        !vars.contains(&"y"),
        "function-body local should not emit Variable: {:?}",
        g.nodes
    );
}

#[test]
fn class_body_attribute_does_not_emit_variable() {
    // class-body assignments are captured as Property, not Variable.
    let g = parse("class C:\n    attr = 1\n");
    let vars = variables(&g);
    assert!(
        !vars.contains(&"attr"),
        "class-body assignment should not emit Variable: {:?}",
        g.nodes
    );
}

#[test]
fn nested_if_local_does_not_emit() {
    let g = parse("def f():\n    if cond:\n        inner = 3\n");
    let vars = variables(&g);
    assert!(
        !vars.contains(&"inner"),
        "block-scope assignment should not emit Variable: {:?}",
        g.nodes
    );
}

#[test]
fn module_level_with_function_body_only_top_emits() {
    let g = parse("outer = 1\ndef f():\n    inner = 2\n");
    let vars = variables(&g);
    assert_eq!(vars, vec!["outer"], "nodes: {:?}", g.nodes);
}
