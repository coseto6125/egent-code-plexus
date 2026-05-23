//! Python Protocol/ABC base-list detection — spec §4.5b.
//!
//! When a class's base list contains ONLY Protocol-markers
//! (`Protocol`, `ABC`, `ABCMeta` — bare or dotted, parametrized or plain),
//! the parser promotes `NodeKind::Class` → `NodeKind::Interface`.
//! Mixed bases and bare classes stay `NodeKind::Class`.

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

fn node_kind(g: &LocalGraph, name: &str) -> NodeKind {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("node {name} not found; nodes = {:?}", g.nodes))
        .kind
}

#[test]
fn all_protocol_promotes_to_interface() {
    let g = parse("class Foo(Protocol): pass\n");
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Interface);
}

#[test]
fn dotted_typing_protocol_promotes() {
    let g = parse("import typing\nclass Foo(typing.Protocol): pass\n");
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Interface);
}

#[test]
fn all_abc_promotes() {
    let g = parse("from abc import ABC\nclass Foo(ABC): pass\n");
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Interface);
}

#[test]
fn abcmeta_promotes() {
    // ABCMeta as a base (not metaclass= kwarg) is a Protocol-marker.
    // NOTE: `class Foo(metaclass=ABCMeta)` uses a keyword argument that the
    // current tree-sitter heritage capture does not pick up (it captures
    // positional expression nodes only). That kwarg form is deferred —
    // see TODO below.
    // TODO: §4.5b — metaclass=ABCMeta detection deferred (PR scope);
    //   kwarg nodes in argument_list are not captured by the `@heritage`
    //   pattern in queries.scm which matches `(expression)`, not keyword args.
    let g = parse("from abc import ABCMeta\nclass Foo(ABCMeta): pass\n");
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Interface);
}

#[test]
fn parametrized_protocol_promotes() {
    let g = parse(
        "from typing import Protocol, TypeVar\nT = TypeVar(\"T\")\nclass Foo(Protocol[T]): pass\n",
    );
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Interface);
}

#[test]
fn mixed_concrete_and_protocol_stays_class() {
    // Bar is undefined / concrete — mixed base keeps NodeKind::Class
    // to preserve concrete inheritance semantics for the Bar→Foo edge.
    let g = parse("class Foo(Bar, Protocol): pass\n");
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Class);
}

#[test]
fn generic_alone_stays_class() {
    // Generic[T] is parameterization, not an interface contract.
    let g = parse(
        "from typing import Generic, TypeVar\nT = TypeVar(\"T\")\nclass Foo(Generic[T]): pass\n",
    );
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Class);
}

#[test]
fn bare_class_stays_class() {
    let g = parse("class Foo: pass\n");
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Class);
}

#[test]
fn multiple_protocols_promote() {
    // Two Protocol-markers, no concrete base — should promote.
    let g = parse("class Foo(Protocol, ABC): pass\n");
    assert_eq!(node_kind(&g, "Foo"), NodeKind::Interface);
}
