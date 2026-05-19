//! Verifies that Dart `mixin` declarations emit NodeKind::Trait (not Interface).
//! Mixins are default-method containers, matching the Kotlin mixin convention.

use cgn_analyzer::dart::parser::DartProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = DartProvider::new().expect("provider");
    p.parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse")
}

fn has(graph: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    graph.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

#[test]
fn simple_mixin_emits_trait() {
    let g = parse("mixin Flyable { void fly() {} }");
    assert!(
        has(&g, "Flyable", NodeKind::Trait),
        "`Flyable` mixin must be NodeKind::Trait; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn mixin_not_emitted_as_interface() {
    let g = parse("mixin Flyable { void fly() {} }");
    assert!(
        !has(&g, "Flyable", NodeKind::Interface),
        "`Flyable` mixin must not be NodeKind::Interface; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn mixin_on_constraint_emits_trait() {
    let g = parse("mixin HydratedMixin<State> on BlocBase<State> { void hydrate() {} }");
    assert!(
        has(&g, "HydratedMixin", NodeKind::Trait),
        "`HydratedMixin` mixin must be NodeKind::Trait; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn private_mixin_is_not_exported() {
    let g = parse("mixin _Internal { void doWork() {} }");
    let node = g
        .nodes
        .iter()
        .find(|n| n.name == "_Internal" && n.kind == NodeKind::Trait)
        .expect("`_Internal` Trait node missing");
    assert!(!node.is_exported, "`_Internal` mixin must not be exported");
}

#[test]
fn enum_emits_enum_not_interface() {
    // Enums now emit NodeKind::Enum (was incorrectly NodeKind::Interface before
    // the fix). Mixin-trait refactor must not regress this.
    let g = parse("enum Color { red, green, blue }");
    assert!(
        has(&g, "Color", NodeKind::Enum),
        "`Color` enum must be NodeKind::Enum; nodes: {:#?}",
        g.nodes
    );
    assert!(
        !has(&g, "Color", NodeKind::Interface),
        "`Color` enum must not be NodeKind::Interface; nodes: {:#?}",
        g.nodes
    );
}
