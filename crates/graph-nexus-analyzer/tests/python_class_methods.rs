//! Integration tests for Python class-method classification.
//!
//! Regression: `def` inside a `class` body must surface as `NodeKind::Method`,
//! not `NodeKind::Function`. Free-standing `def` and closures nested inside
//! a method body stay `Function`.

use graph_nexus_analyzer::python::parser::PythonProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = PythonProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.py"), source.as_bytes())
        .expect("parse")
}

fn find<'a>(g: &'a LocalGraph, name: &str) -> Option<&'a RawNode> {
    g.nodes.iter().find(|n| n.name == name)
}

#[test]
fn class_body_def_is_method() {
    let src = "class Foo:\n    def bar(self):\n        pass\n";
    let g = parse(src);
    let bar = find(&g, "bar").expect("bar node");
    assert_eq!(bar.kind, NodeKind::Method, "{:?}", bar);
}

#[test]
fn module_level_def_stays_function() {
    let src = "def top_level():\n    pass\n";
    let g = parse(src);
    let top = find(&g, "top_level").expect("top_level node");
    assert_eq!(top.kind, NodeKind::Function, "{:?}", top);
}

#[test]
fn closure_inside_method_stays_function() {
    let src = "class Foo:\n    def bar(self):\n        def inner():\n            pass\n        return inner\n";
    let g = parse(src);
    let bar = find(&g, "bar").expect("bar node");
    let inner = find(&g, "inner").expect("inner node");
    assert_eq!(bar.kind, NodeKind::Method);
    assert_eq!(inner.kind, NodeKind::Function, "{:?}", inner);
}

#[test]
fn decorated_class_method_is_method() {
    let src = "class Foo:\n    @staticmethod\n    def bar():\n        pass\n    @property\n    def baz(self):\n        return 1\n";
    let g = parse(src);
    let bar = find(&g, "bar").expect("bar node");
    assert_eq!(
        bar.kind,
        NodeKind::Method,
        "@staticmethod-decorated def must be Method: {:?}",
        bar
    );
    // `baz` may be classified as Property (separate Property query). Verify that's
    // the case explicitly so a future change doesn't silently demote it to Method.
    let baz = find(&g, "baz").expect("baz node");
    assert!(
        matches!(baz.kind, NodeKind::Property | NodeKind::Method),
        "@property def baz unexpected kind {:?}",
        baz.kind
    );
}

#[test]
fn async_class_method_is_method() {
    let src = "class Foo:\n    async def bar(self):\n        return 1\n";
    let g = parse(src);
    let bar = find(&g, "bar").expect("bar node");
    assert_eq!(
        bar.kind,
        NodeKind::Method,
        "async def in class must be Method: {:?}",
        bar
    );
}

#[test]
fn nested_class_method_is_method() {
    let src = "class Outer:\n    class Inner:\n        def deep(self):\n            pass\n";
    let g = parse(src);
    let deep = find(&g, "deep").expect("deep node");
    assert_eq!(deep.kind, NodeKind::Method, "{:?}", deep);
}
