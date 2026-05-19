//! Per-symbol underscore visibility checks for the Dart provider.
//!
//! In Dart, identifiers starting with `_` are file-private regardless of
//! `library` directive. The provider must reflect this on every emitted
//! RawNode via `is_exported = !name.starts_with('_')`.
//!
//! Covers Wave 3 / Matrix A1 (Dart row) from
//! `docs/specs/2026-05-15-matrix-optimization-opportunities.md`.

use cgn_analyzer::dart::parser::DartProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = DartProvider::new().expect("DartProvider init");
    let graph = provider
        .parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn public_function_is_exported() {
    let nodes = parse("void foo() {}");
    let foo = find(&nodes, "foo", NodeKind::Function);
    assert!(foo.is_exported, "`foo` must be exported");
}

#[test]
fn private_function_is_not_exported() {
    let nodes = parse("void _bar() {}");
    let bar = find(&nodes, "_bar", NodeKind::Function);
    assert!(!bar.is_exported, "`_bar` must be private");
}

#[test]
fn public_class_with_private_method() {
    let src = "class C { void pub() {} void _priv() {} }";
    let nodes = parse(src);

    let class_c = find(&nodes, "C", NodeKind::Class);
    assert!(class_c.is_exported, "class `C` must be exported");

    let pub_m = find(&nodes, "pub", NodeKind::Method);
    assert!(pub_m.is_exported, "method `pub` must be exported");

    let priv_m = find(&nodes, "_priv", NodeKind::Method);
    assert!(!priv_m.is_exported, "method `_priv` must be private");
}

#[test]
fn private_class_is_not_exported() {
    let nodes = parse("class _Hidden {}");
    let hidden = find(&nodes, "_Hidden", NodeKind::Class);
    assert!(!hidden.is_exported, "class `_Hidden` must be private");
}

#[test]
fn property_visibility_in_class() {
    // tree-sitter-dart represents class-body `final p = 1;` as
    // `declaration > initialized_identifier_list > initialized_identifier`,
    // which is what `queries.scm` matches for `@property`.
    let src = "class C { final p = 1; final _p = 2; }";
    let nodes = parse(src);

    let p = find(&nodes, "p", NodeKind::Property);
    assert!(p.is_exported, "property `p` must be exported");

    let _p = find(&nodes, "_p", NodeKind::Property);
    assert!(!_p.is_exported, "property `_p` must be private");
}

#[test]
fn private_enum_is_not_exported() {
    // Enums emit NodeKind::Enum (corrected from the prior Interface mis-mapping).
    let src = "enum _Color { red, blue }";
    let nodes = parse(src);
    let e = find(&nodes, "_Color", NodeKind::Enum);
    assert!(!e.is_exported, "enum `_Color` must be private");
}
