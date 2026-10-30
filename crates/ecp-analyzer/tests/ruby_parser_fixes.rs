//! Ruby parser fixes: visibility, mixin owner on shared rows, tx scope order.

mod driver_parity_support;

use driver_parity_support::tx;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    RubyProvider::new()
        .expect("RubyProvider::new")
        .parse_file(Path::new("a.rb"), src.as_bytes())
        .expect("parse_file")
}

fn named<'g>(g: &'g LocalGraph, name: &str) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node {name}"))
}

// FU-2026-10-30-e444dfa8c963
#[test]
fn test_ruby_visibility_two_methods_on_one_row_keep_their_own_visibility() {
    let g = parse("class A; def pub; end; private; def priv; end; end\n");
    assert!(named(&g, "pub").is_exported, "public before `private`");
    assert!(!named(&g, "priv").is_exported, "private after `private`");
}

// FU-2026-10-30-e444dfa8c963
#[test]
fn test_ruby_private_with_symbol_argument_hides_only_that_method() {
    let g = parse("class A\n  def foo; end\n  def bar; end\n  private :foo\nend\n");
    assert!(!named(&g, "foo").is_exported);
    assert!(named(&g, "bar").is_exported);
}

// FU-2026-10-30-e444dfa8c963
#[test]
fn test_ruby_private_with_several_symbols_hides_each_named_method() {
    let g = parse("class A\n  def a; end\n  def b; end\n  def c; end\n  private :a, :c\nend\n");
    assert!(!named(&g, "a").is_exported);
    assert!(named(&g, "b").is_exported);
    assert!(!named(&g, "c").is_exported);
}

// FU-2026-10-30-e444dfa8c963 (capability: `private def` inline form)
#[test]
fn test_ruby_private_def_inline_hides_that_method_only() {
    let g = parse("class A\n  private def hid; end\n  def shown; end\nend\n");
    assert!(!named(&g, "hid").is_exported);
    assert!(named(&g, "shown").is_exported);
}

// FU-2026-10-30-e444dfa8c963 guard: bare marker keeps its section semantics.
#[test]
fn test_ruby_bare_private_marker_then_public_marker_sections_stay() {
    let g = parse("class A\n  def a; end\n  private\n  def b; end\n  public\n  def c; end\nend\n");
    assert!(named(&g, "a").is_exported);
    assert!(!named(&g, "b").is_exported);
    assert!(named(&g, "c").is_exported);
}

// FU-2026-10-30-e444dfa8c963
#[test]
fn test_ruby_include_in_second_class_on_same_row_binds_that_class() {
    let g = parse("class A; end; class B; include M; end\n");
    assert_eq!(named(&g, "B").heritage, ["M"]);
    assert!(
        named(&g, "A").heritage.is_empty(),
        "{:?}",
        named(&g, "A").heritage
    );
}

// FU-2026-10-30-e444dfa8c963 guard: multi-row classes bind by containment.
#[test]
fn test_ruby_include_in_multirow_class_binds_enclosing_class() {
    let g = parse("class A\nend\nclass B\n  include M\nend\n");
    assert_eq!(named(&g, "B").heritage, ["M"]);
    assert!(named(&g, "A").heritage.is_empty());
}

fn tx_src(count: usize) -> String {
    let mut s = String::from("class Repo\n");
    for i in 0..count {
        s.push_str(&format!("  def m{i}\n    transaction do\n    end\n  end\n"));
    }
    s.push_str("end\n");
    s
}

// FU-2026-10-30-d5c65be8aa81
#[test]
fn test_ruby_tx_scopes_follow_node_order_for_many_functions() {
    let g = parse(&tx_src(6));
    let got: Vec<&str> = tx(&g).into_iter().map(|(name, _)| name).collect();
    assert_eq!(got, ["m0", "m1", "m2", "m3", "m4", "m5"]);
    assert!(tx(&g)
        .iter()
        .all(|(_, fw)| *fw == FrameworkId::RubyActiveRecordTransaction));
    assert!(g.nodes.iter().any(|n| n.kind == NodeKind::Class));
}

// FU-2026-10-30-d5c65be8aa81
#[test]
fn test_ruby_tx_scopes_repeat_parse_gives_identical_order() {
    let src = tx_src(8);
    let first: Vec<String> = tx(&parse(&src))
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();
    for _ in 0..30 {
        let again: Vec<String> = tx(&parse(&src))
            .iter()
            .map(|(n, _)| n.to_string())
            .collect();
        assert_eq!(first, again);
    }
}

// FU-2026-10-30-d5c65be8aa81 guard: two transaction blocks in one function emit one scope.
#[test]
fn test_ruby_tx_scopes_two_blocks_in_one_function_emit_one_scope() {
    let g = parse("def a\n  transaction do\n  end\n  transaction do\n  end\nend\n");
    assert_eq!(tx(&g).len(), 1);
}
