use cgn_analyzer::ruby::parser::RubyProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = RubyProvider::new().expect("provider");
    p.parse_file(Path::new("test.rb"), src.as_bytes())
        .expect("parse")
}

#[test]
fn module_emits_trait_kind() {
    let g = parse("module Foo\n  def bar; end\nend\n");
    let foo = g.nodes.iter().find(|n| n.name == "Foo").expect("Foo missing");
    assert_eq!(foo.kind, NodeKind::Trait, "got {:?}", foo);
}

#[test]
fn class_still_emits_class_kind() {
    let g = parse("class Foo\n  def bar; end\nend\n");
    let foo = g.nodes.iter().find(|n| n.name == "Foo").expect("Foo missing");
    assert_eq!(foo.kind, NodeKind::Class, "got {:?}", foo);
}

#[test]
fn nested_module_emits_trait_kind() {
    let g = parse("module Outer\n  module Inner\n    def x; end\n  end\nend\n");
    let inner = g
        .nodes
        .iter()
        .find(|n| n.name == "Inner")
        .expect("Inner missing");
    assert_eq!(inner.kind, NodeKind::Trait, "got {:?}", inner);
}
