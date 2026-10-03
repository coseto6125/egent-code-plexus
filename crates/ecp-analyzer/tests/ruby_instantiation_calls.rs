//! Ruby instantiation calls: `A.new` must give `ecp impact` a caller for `A`.
//! The callee is captured as `A.new`; nothing maps it to `A` or to
//! `initialize`, so today it resolves nothing.

mod instantiation_calls_support;

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_no_instantiation_call, assert_only_call,
    graph_of,
};
use std::path::Path;

const WIDGET: (&str, &str) = (
    "lib/widget.rb",
    "class Widget\n  def initialize(x)\n    @x = x\n  end\n\n  def self.create\n    1\n  end\nend\n",
);
const GADGET: (&str, &str) = ("lib/gadget.rb", "class Gadget\nend\n");
const MAKE_WIDGET: (&str, &str) = (
    "lib/app.rb",
    "require_relative 'widget'\n\nclass App\n  def make_widget\n    Widget.new(1)\n  end\nend\n",
);

fn provider() -> RubyProvider {
    RubyProvider::new().expect("RubyProvider::new")
}

#[test]
fn test_ruby_cross_file_new_with_initialize_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "make_widget", "Widget", "lib/widget.rb");
}

#[test]
fn test_ruby_cross_file_new_without_initialize_calls_class() {
    let app = (
        "lib/app.rb",
        "require_relative 'gadget'\n\nclass App\n  def make_gadget\n    Gadget.new\n  end\nend\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "make_gadget",
        "Gadget",
        NodeKind::Class,
        "lib/gadget.rb",
    );
}

#[test]
fn test_ruby_same_file_new_calls_constructor() {
    let local = (
        "lib/local.rb",
        "class Local\n  def initialize\n    @ready = true\n  end\nend\n\nclass LocalApp\n  def make_local\n    Local.new\n  end\nend\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "make_local", "Local", "lib/local.rb");
}

#[test]
fn test_ruby_namespaced_new_calls_constructor() {
    let item = (
        "lib/shop/item.rb",
        "module Shop\n  class Item\n    def initialize\n      @ready = true\n    end\n  end\nend\n",
    );
    let app = (
        "lib/app.rb",
        "require_relative 'shop/item'\n\nclass App\n  def make_item\n    Shop::Item.new\n  end\nend\n",
    );
    let graph = graph_of(&provider(), &[item, app]);
    assert_calls_constructor(&graph, "make_item", "Item", "lib/shop/item.rb");
}

/// A class method other than `new` stays an ordinary method call.
#[test]
fn test_ruby_class_method_call_targets_method() {
    let app = (
        "lib/app.rb",
        "require_relative 'widget'\n\nclass App\n  def build_widget\n    Widget.create\n  end\nend\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_no_instantiation_call(&graph, "build_widget", "Widget");
    assert_only_call(&graph, "build_widget", "create", "lib/widget.rb");
}

/// Raw callee strings of the node named `caller`, sorted.
fn raw_calls(src: &str, caller: &str) -> Vec<String> {
    let local = provider()
        .parse_file(Path::new("lib/app.rb"), src.as_bytes())
        .expect("parse lib/app.rb");
    let mut calls = local
        .nodes
        .iter()
        .find(|n| n.name == caller)
        .unwrap_or_else(|| panic!("no node `{caller}`"))
        .calls
        .clone();
    calls.sort();
    calls
}

/// Only `.new` reads a scoped receiver: `Shop::Item.save` keeps the bare
/// `save` callee that the resolver binds today, and so does `.save` on a
/// `Shop::Item.new` chain.
#[test]
fn test_ruby_scoped_receiver_method_call_keeps_bare_callee() {
    let save = "class App\n  def save_item\n    Shop::Item.save\n  end\nend\n";
    assert_eq!(raw_calls(save, "save_item"), ["save"]);

    let chain = "class App\n  def save_new\n    Shop::Item.new.save\n  end\nend\n";
    assert_eq!(raw_calls(chain, "save_new"), ["Shop::Item.new", "save"]);
}
