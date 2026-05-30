//! Ruby class-level methods must carry `is_static` regardless of which syntax
//! declares them: `def self.foo` (singleton_method) and `class << self; def foo`
//! (method nested under singleton_class) are equivalent. The latter relies on a
//! `parent().parent() == singleton_class` walk in `function_meta::ruby` that is
//! easy to break when the surrounding grammar handling changes — this test pins
//! the behaviour so a regression surfaces immediately.

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{FunctionMeta, NodeKind};
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = RubyProvider::new().expect("provider");
    p.parse_file(Path::new("x.rb"), src.as_bytes())
        .expect("parse")
}

/// `is_static` for the Method node named `name`, paired with its FunctionMeta by span.
fn is_static(g: &LocalGraph, name: &str) -> bool {
    let node = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name == name)
        .unwrap_or_else(|| panic!("no Method node named {name}, nodes: {:?}", g.nodes));
    g.raw_function_metas
        .iter()
        .find(|m| m.span == node.span)
        .map(|m| m.flags & FunctionMeta::FLAG_STATIC != 0)
        .unwrap_or_else(|| panic!("no FunctionMeta for {name} at span {:?}", node.span))
}

#[test]
fn def_self_method_is_static() {
    let g = parse("class Foo\n  def self.build; end\nend\n");
    assert!(is_static(&g, "build"), "def self.build must be is_static");
}

#[test]
fn singleton_class_method_is_static() {
    let g = parse("class Foo\n  class << self\n    def build; end\n  end\nend\n");
    assert!(
        is_static(&g, "build"),
        "method inside `class << self` must be is_static"
    );
}

#[test]
fn multiple_singleton_class_methods_all_static() {
    let g = parse("class Foo\n  class << self\n    def a; end\n    def b; end\n  end\nend\n");
    assert!(
        is_static(&g, "a"),
        "first class<<self method must be static"
    );
    assert!(
        is_static(&g, "b"),
        "second class<<self method must be static"
    );
}

#[test]
fn instance_method_is_not_static() {
    let g = parse("class Foo\n  def run; end\nend\n");
    assert!(
        !is_static(&g, "run"),
        "plain instance method must not be is_static"
    );
}

#[test]
fn mixed_class_pins_each_method_flag() {
    let g = parse(
        "class Foo\n  def inst; end\n  def self.cls1; end\n  class << self\n    def cls2; end\n  end\nend\n",
    );
    assert!(!is_static(&g, "inst"), "inst is instance");
    assert!(is_static(&g, "cls1"), "cls1 is def self.");
    assert!(is_static(&g, "cls2"), "cls2 is class<<self");
}
