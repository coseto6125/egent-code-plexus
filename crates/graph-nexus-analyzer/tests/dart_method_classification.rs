//! Verifies that Dart class methods, top-level functions, and constructors
//! are classified with the correct NodeKind.

use graph_nexus_analyzer::dart::parser::DartProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = DartProvider::new().expect("provider");
    p.parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse")
}

fn find(graph: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    graph.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

#[test]
fn class_method_emits_method() {
    let g = parse("class Foo { void bar() {} }");
    assert!(
        find(&g, "bar", NodeKind::Method),
        "`bar` inside class Foo must be NodeKind::Method; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn top_level_function_emits_function() {
    let g = parse("void topLevel() {}");
    assert!(
        find(&g, "topLevel", NodeKind::Function),
        "`topLevel` at file scope must be NodeKind::Function; nodes: {:#?}",
        g.nodes
    );
    // Regression: must not also appear as Method
    assert!(
        !find(&g, "topLevel", NodeKind::Method),
        "`topLevel` must not be NodeKind::Method"
    );
}

#[test]
fn constructor_emits_constructor() {
    let g = parse("class Foo { Foo() {} }");
    assert!(
        find(&g, "Foo", NodeKind::Constructor),
        "constructor `Foo` must be NodeKind::Constructor; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn class_with_method_and_constructor() {
    let g = parse("class Foo { Foo() {} void bar() {} }");
    assert!(
        find(&g, "Foo", NodeKind::Constructor),
        "constructor `Foo` must be NodeKind::Constructor; nodes: {:#?}",
        g.nodes
    );
    assert!(
        find(&g, "bar", NodeKind::Method),
        "`bar` must be NodeKind::Method; nodes: {:#?}",
        g.nodes
    );
    // Class itself
    assert!(
        find(&g, "Foo", NodeKind::Class),
        "class `Foo` must be NodeKind::Class; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn private_class_method_is_not_exported() {
    let g = parse("class Foo { void _secret() {} }");
    let node = g
        .nodes
        .iter()
        .find(|n| n.name == "_secret" && n.kind == NodeKind::Method)
        .expect("_secret method missing");
    assert!(!node.is_exported, "`_secret` must not be exported");
}
