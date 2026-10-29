//! `RubyProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order where the order is fixed by
//! the step list.

mod driver_parity_support;

use driver_parity_support::{assert_empty_graph, names_kinds, Span4};
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

fn parse_at(path: &str, src: &[u8]) -> LocalGraph {
    RubyProvider::new()
        .expect("RubyProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn parse(src: &[u8]) -> LocalGraph {
    parse_at("a.rb", src)
}

fn named<'g>(g: &'g LocalGraph, name: &str) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node `{name}` in {:?}", names_kinds(g)))
}

/// `(source, imported_name, alias)`
type ImportRow<'g> = (&'g str, &'g str, Option<&'g str>);

fn imports(g: &LocalGraph) -> Vec<ImportRow<'_>> {
    g.imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect()
}

fn blind(g: &LocalGraph) -> Vec<(&str, Span4)> {
    g.blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.span))
        .collect()
}

#[test]
fn test_ruby_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_empty_graph(&g, "a.rb");
}

/// `require` and `require_relative` each push one import in source order,
/// with the string content as both source and name; no sort, no node.
#[test]
fn test_ruby_driver_imports_only_file_keeps_source_order() {
    let g = parse(b"require \"json\"\nrequire_relative \"lib/helper\"\n");
    assert!(g.nodes.is_empty(), "{:?}", names_kinds(&g));
    assert_eq!(
        imports(&g),
        [("json", "json", None), ("lib/helper", "lib/helper", None)]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
    assert!(g.blind_spots.is_empty() && g.routes.is_empty());
}

/// The root capture gives span and hash, `@name` the name; `initialize`
/// becomes a Constructor and the superclass is the class's heritage.
#[test]
fn test_ruby_driver_class_with_methods_emits_root_spans() {
    let src: &[u8] =
        b"class Dog < Animal\n  def initialize\n  end\n\n  def bark\n    puts \"woof\"\n  end\nend\n";
    let g = parse(src);
    let class = named(&g, "Dog");
    assert_eq!(
        (class.kind, class.span, class.is_exported),
        (NodeKind::Class, (0, 0, 7, 3), true)
    );
    assert_eq!(class.heritage, ["Animal"]);
    assert_eq!(
        class.content_hash,
        xxh3_64_bytes(&src[..src.len() - 1]),
        "the class root, without the trailing newline"
    );
    let init = named(&g, "initialize");
    assert_eq!(
        (init.kind, init.span, init.is_exported),
        (NodeKind::Constructor, (1, 2, 2, 5), true)
    );
    let bark = named(&g, "bark");
    assert_eq!(
        (bark.kind, bark.span, bark.is_exported),
        (NodeKind::Method, (4, 2, 6, 5), true)
    );
    assert_eq!(bark.owner_class.as_deref(), Some("Dog"));
    assert!(bark.decorators.is_empty() && bark.heritage.is_empty());
    assert_eq!(
        bark.content_hash,
        xxh3_64_bytes(b"def bark\n    puts \"woof\"\n  end")
    );
}

/// Two scalar constants and no method make the module an enum imitation:
/// the walk after the query pushes one blind spot for the whole module.
#[test]
fn test_ruby_driver_module_with_constants_emits_consts_and_enum_blind_spot() {
    let g = parse(b"module Color\n  RED = 1\n  BLUE = 2\nend\n");
    let module = named(&g, "Color");
    assert_eq!(
        (module.kind, module.span, module.is_exported),
        (NodeKind::Trait, (0, 0, 3, 3), true)
    );
    let red = named(&g, "RED");
    assert_eq!(
        (red.kind, red.span, red.is_exported),
        (NodeKind::Const, (1, 2, 1, 9), true)
    );
    assert_eq!(red.content_hash, xxh3_64_bytes(b"RED = 1"));
    assert_eq!(named(&g, "BLUE").span, (2, 2, 2, 10));
    assert!(g.imports.is_empty(), "scalar right sides bind no alias");
    assert_eq!(blind(&g), [("ruby-module-as-enum", (0, 0, 3, 3))]);
}

/// `eval` is a `BLIND` row; `send` is a role that pushes only for a
/// non-literal target. Both push inside the query, in source order.
#[test]
fn test_ruby_driver_send_literal_skips_and_dynamic_emits_blind_spot() {
    let g = parse(
        b"def run(obj, name, code)\n  eval(code)\n  obj.send(:to_s)\n  obj.send(name)\nend\n",
    );
    assert_eq!(
        blind(&g),
        [("rb-eval", (1, 2, 1, 12)), ("rb-send", (3, 2, 3, 16))]
    );
    assert!(g.blind_spots.iter().all(|b| !b.is_test));
    assert!(g
        .blind_spots
        .iter()
        .all(|b| b.file_path == Path::new("a.rb")));
}

#[test]
fn test_ruby_driver_include_appends_mixin_to_class_heritage() {
    let g = parse(b"class Dog\n  include Walkable\nend\n");
    assert_eq!(names_kinds(&g), [("Dog", NodeKind::Class)]);
    assert_eq!(g.nodes[0].heritage, ["Walkable"]);
    assert_eq!(g.nodes[0].span, (0, 0, 2, 3));
}

/// The bindings step runs after the query, so the delegated Method comes
/// after the class: a zero-width span at the call row and a zero hash.
#[test]
fn test_ruby_driver_delegate_emits_binding_and_trailing_method() {
    let g = parse(b"class Order\n  delegate :name, to: :customer\nend\n");
    assert_eq!(
        names_kinds(&g),
        [("Order", NodeKind::Class), ("name", NodeKind::Method)]
    );
    let m = &g.nodes[1];
    assert_eq!(
        (m.span, m.is_exported, m.content_hash),
        ((1, 0, 1, 0), true, 0)
    );
    assert!(m.heritage.is_empty() && m.decorators.is_empty() && m.type_annotation.is_none());
    assert_eq!(imports(&g), [("customer.name", "name", Some("name"))]);
}

/// The visibility walk runs before the query: a method after a bare
/// `private` is not exported, one before it is.
#[test]
fn test_ruby_driver_private_section_unexports_following_method() {
    let g = parse(b"class Svc\n  def visible\n  end\n\n  private\n\n  def hidden\n  end\nend\n");
    let visible = named(&g, "visible");
    assert_eq!((visible.span, visible.is_exported), ((1, 2, 2, 5), true));
    let hidden = named(&g, "hidden");
    assert_eq!((hidden.span, hidden.is_exported), ((6, 2, 7, 5), false));
    assert!(named(&g, "Svc").is_exported);
}

/// The generic `get` capture inside `routes.draw` is dropped and the Rails
/// expansion supplies the route; the draw block with a call becomes an
/// `<anonymous>` node.
#[test]
fn test_ruby_driver_rails_route_file_replaces_generic_capture() {
    let g = parse_at(
        "config/routes.rb",
        b"Rails.application.routes.draw do\n  get \"/health\", to: \"health#show\"\nend\n",
    );
    let routes: Vec<_> = g
        .routes
        .iter()
        .map(|r| (r.method.as_str(), r.path.as_str(), r.handler_name(), r.span))
        .collect();
    assert_eq!(
        routes,
        [("GET", "/health", Some("health#show"), (1, 2, 1, 34))]
    );
    assert_eq!(names_kinds(&g), [("<anonymous:1:30>", NodeKind::Function)]);
    assert_eq!(g.nodes[0].span, (0, 30, 2, 3));
    assert!(g.blind_spots.is_empty());
}

/// A block whose body holds a call gets an `<anonymous>` node named by its
/// 1-based row; a block with no call gets none.
#[test]
fn test_ruby_driver_anonymous_block_emits_only_with_call() {
    let g =
        parse(b"def run(items)\n  items.each { |i| process(i) }\n  items.map { |i| i * 2 }\nend\n");
    let anon: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("<anonymous"))
        .map(|n| (n.name.as_str(), n.kind, n.span))
        .collect();
    assert_eq!(
        anon,
        [("<anonymous:2:13>", NodeKind::Function, (1, 13, 1, 31))]
    );
    let a = named(&g, "<anonymous:2:13>");
    assert_eq!(a.content_hash, xxh3_64_bytes(b"{ |i| process(i) }"));
    assert!(!a.is_exported);
}
