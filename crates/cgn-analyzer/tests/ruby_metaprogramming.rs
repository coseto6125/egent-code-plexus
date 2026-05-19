//! Integration tests for Ruby `attr_*` metaprogramming and mixin tracking.
//!
//! See `docs/specs/2026-05-15-matrix-optimization-opportunities.md` §A1 (Ruby)
//! + §A4 (Ruby mixins).

use graph_nexus_analyzer::ruby::parser::RubyProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = RubyProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.rb"), source.as_bytes())
        .expect("parse")
}

fn find_class<'a>(graph: &'a LocalGraph, name: &str) -> &'a RawNode {
    graph
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == name)
        .unwrap_or_else(|| panic!("class {name} not found; nodes = {:?}", graph.nodes))
}

fn properties_of<'a>(graph: &'a LocalGraph, class: &RawNode) -> Vec<&'a RawNode> {
    let (cs, _ccol, ce, _ecol) = class.span;
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property && n.span.0 >= cs && n.span.2 <= ce)
        .collect()
}

#[test]
fn attr_reader_emits_properties() {
    let graph = parse("class C\n  attr_reader :foo, :bar\nend\n");
    let c = find_class(&graph, "C");
    let props = properties_of(&graph, c);
    let names: Vec<&str> = props.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(props.len(), 2, "expected 2 properties, got {names:?}");
    assert!(names.contains(&"foo"), "missing foo: {names:?}");
    assert!(names.contains(&"bar"), "missing bar: {names:?}");
    for p in &props {
        assert!(p.is_exported, "{} should be exported", p.name);
    }
}

#[test]
fn attr_writer_emits_properties() {
    let graph = parse("class C\n  attr_writer :baz\nend\n");
    let c = find_class(&graph, "C");
    let props = properties_of(&graph, c);
    let names: Vec<&str> = props.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["baz"]);
}

#[test]
fn attr_accessor_emits_properties() {
    let graph = parse("class C\n  attr_accessor :name\nend\n");
    let c = find_class(&graph, "C");
    let props = properties_of(&graph, c);
    let names: Vec<&str> = props.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["name"]);
}

#[test]
fn include_adds_mixin_to_heritage() {
    let graph = parse("class C\n  include Enumerable\nend\n");
    let c = find_class(&graph, "C");
    assert!(
        c.heritage.iter().any(|h| h == "Enumerable"),
        "heritage missing Enumerable: {:?}",
        c.heritage
    );
}

#[test]
fn extend_adds_mixin_to_heritage() {
    let graph = parse("class C\n  extend Forwardable\nend\n");
    let c = find_class(&graph, "C");
    assert!(
        c.heritage.iter().any(|h| h == "Forwardable"),
        "heritage missing Forwardable: {:?}",
        c.heritage
    );
}

#[test]
fn combined_superclass_mixins_and_attr() {
    let source = "class C < Parent\n  include M1\n  include M2\n  attr_accessor :x\nend\n";
    let graph = parse(source);
    let c = find_class(&graph, "C");
    assert_eq!(
        c.heritage,
        vec!["Parent".to_string(), "M1".to_string(), "M2".to_string()],
        "unexpected heritage: {:?}",
        c.heritage
    );
    let props = properties_of(&graph, c);
    let names: Vec<&str> = props.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["x"]);
}

#[test]
fn attr_outside_class_is_noop() {
    // Behavior: `attr_reader` at the top level still parses as a `call`, so
    // the query fires and Property nodes are emitted. They simply have no
    // enclosing Class — downstream resolution will treat them as file-level
    // properties. This test documents that chosen behavior.
    let graph = parse("attr_reader :stray\n");
    let stray: Vec<&RawNode> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property && n.name == "stray")
        .collect();
    assert_eq!(stray.len(), 1, "expected 1 top-level Property `stray`");
}
