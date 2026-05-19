//! Verifies that Dart class methods, top-level functions, and constructors
//! are classified with the correct NodeKind.

use cgn_analyzer::dart::parser::DartProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
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
    // Regression hardening: the bare `(function_signature ...) @function`
    // pattern in queries.scm matches the method's inner function_signature
    // node too — parser.rs filters that case out by checking parent.kind
    // ∈ {function_declaration, method_signature}. If anyone deletes that
    // filter, `bar` would silently double-emit as Function + Method.
    assert!(
        !find(&g, "bar", NodeKind::Function),
        "`bar` is a class method and must not also appear as Function; nodes: {:#?}",
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
    // Regression hardening (matches class_method_emits_method): neither
    // the method `bar` nor the constructor `Foo` should leak through the
    // bare `function_signature` capture as Function. The parser.rs
    // parent.kind filter blocks the method path; the constructor uses a
    // separate `constructor_signature` AST node entirely.
    assert!(
        !find(&g, "bar", NodeKind::Function),
        "`bar` is a class method and must not also appear as Function; nodes: {:#?}",
        g.nodes
    );
    assert!(
        !find(&g, "Foo", NodeKind::Function),
        "`Foo` is a constructor and must not also appear as Function; nodes: {:#?}",
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
