//! Heritage capture for Ruby class superclass.
//!
//! Verifies the explicit superclass node list in `ruby/queries.scm` —
//! `[(constant) (scope_resolution) (identifier) (call)]` — populates
//! `RawNode.heritage` for all common shapes. The previous list was
//! `[(constant) (scope_resolution) (identifier)]`, missing `(call)` so
//! method-returning superclasses like `class X < make_base(Y)` were
//! dropped.

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = RubyProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.rb"), source.as_bytes())
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
fn constant_superclass_populates_heritage() {
    let g = parse("class Cat < Animal\nend\n");
    assert_eq!(class_heritage(&g, "Cat"), &vec!["Animal".to_string()]);
}

#[test]
fn scope_resolution_superclass_populates_heritage() {
    let g = parse("class Cat < Animals::Mammal\nend\n");
    assert_eq!(
        class_heritage(&g, "Cat"),
        &vec!["Animals::Mammal".to_string()]
    );
}

#[test]
fn call_expression_superclass_populates_heritage() {
    let g = parse("class Cat < make_base(Animal)\nend\n");
    assert_eq!(
        class_heritage(&g, "Cat"),
        &vec!["make_base(Animal)".to_string()]
    );
}
