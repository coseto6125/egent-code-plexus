//! Heritage capture for Python class bases.
//!
//! Verifies the `(expression)` widening in `python/queries.scm` so any
//! superclass expression — bare identifier, dotted attribute, subscript,
//! call — populates `RawNode.heritage`. Before the widening only
//! identifier + attribute shapes were captured.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = PythonProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.py"), source.as_bytes())
        .expect("parse")
}

fn class_heritage<'a>(g: &'a LocalGraph, name: &str) -> &'a Vec<String> {
    let n = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == name)
        .unwrap_or_else(|| panic!("class {name} not found; nodes = {:?}", g.nodes));
    &n.heritage
}

#[test]
fn bare_identifier_base_populates_heritage() {
    let g = parse("class Cat(Animal):\n    pass\n");
    assert_eq!(class_heritage(&g, "Cat"), &vec!["Animal".to_string()]);
}

#[test]
fn dotted_attribute_base_populates_heritage() {
    let g = parse("class Cat(animals.Animal):\n    pass\n");
    assert_eq!(
        class_heritage(&g, "Cat"),
        &vec!["animals.Animal".to_string()]
    );
}

#[test]
fn subscript_base_populates_heritage() {
    let g = parse(
        "from typing import Generic, TypeVar\nT = TypeVar('T')\nclass Box(Generic[T]):\n    pass\n",
    );
    assert_eq!(class_heritage(&g, "Box"), &vec!["Generic[T]".to_string()]);
}

#[test]
fn call_expression_base_populates_heritage() {
    let g = parse("class View(mixin(Base)):\n    pass\n");
    assert_eq!(class_heritage(&g, "View"), &vec!["mixin(Base)".to_string()]);
}

#[test]
fn multiple_bases_capture_all() {
    let g = parse("class Cat(Animal, Mixin):\n    pass\n");
    let mut h = class_heritage(&g, "Cat").clone();
    h.sort();
    assert_eq!(h, vec!["Animal".to_string(), "Mixin".to_string()]);
}
