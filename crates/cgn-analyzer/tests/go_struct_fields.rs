//! Per-field visibility on Go struct declarations.
//!
//! Go's export convention applies uniformly: any identifier starting with
//! an uppercase letter is package-exported. The provider previously only
//! propagated this for top-level decls (struct/interface/func/method) but
//! not for individual struct fields. This file pins that fields now emit
//! `Property` RawNodes with `is_exported` derived from the same rule.
//!
//! Covers Wave 3 / Matrix A1 (Go row) from
//! `docs/specs/2026-05-15-matrix-optimization-opportunities.md`.

use cgn_analyzer::go::parser::GoProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = GoProvider::new().expect("GoProvider init");
    let graph = provider
        .parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find_property<'a>(nodes: &'a [RawNode], name: &str) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == NodeKind::Property)
        .unwrap_or_else(|| panic!("missing Property `{name}` in {nodes:#?}"))
}

#[test]
fn exported_struct_field_is_exported() {
    let src = "package p\ntype User struct {\n  Name string\n}\n";
    let nodes = parse(src);
    let f = find_property(&nodes, "Name");
    assert!(f.is_exported, "`Name` must be exported");
}

#[test]
fn private_struct_field_is_not_exported() {
    let src = "package p\ntype User struct {\n  age int\n}\n";
    let nodes = parse(src);
    let f = find_property(&nodes, "age");
    assert!(!f.is_exported, "`age` must be private");
}

#[test]
fn mixed_visibility_struct_fields() {
    let src = "package p\ntype User struct {\n  Name string\n  age int\n}\n";
    let nodes = parse(src);

    let name = find_property(&nodes, "Name");
    assert!(name.is_exported, "`Name` must be exported");

    let age = find_property(&nodes, "age");
    assert!(!age.is_exported, "`age` must be private");
}

#[test]
fn multi_name_field_declaration_emits_one_property_per_name() {
    // Go syntax: `X, Y int` declares two fields sharing one type. The query
    // must emit a Property for each name independently so the second name's
    // visibility doesn't shadow the first.
    let src = "package p\ntype P struct {\n  X, y int\n}\n";
    let nodes = parse(src);

    let x = find_property(&nodes, "X");
    assert!(x.is_exported, "`X` must be exported");

    let y = find_property(&nodes, "y");
    assert!(!y.is_exported, "`y` must be private");
}

#[test]
fn nested_anonymous_struct_fields_are_captured() {
    // Documented choice: tree-sitter queries match at any depth, so fields
    // of nested anonymous structs are also emitted. This is the simpler
    // and more useful behavior — every field token is reachable as a
    // Property regardless of nesting depth.
    let src = "package p\ntype Outer struct {\n  Inner struct {\n    Z string\n    w int\n  }\n}\n";
    let nodes = parse(src);

    // Outer-level field still captured.
    let inner = find_property(&nodes, "Inner");
    assert!(inner.is_exported, "outer-level `Inner` must be exported");

    // Nested fields also captured.
    let z = find_property(&nodes, "Z");
    assert!(z.is_exported, "nested `Z` must be exported");

    let w = find_property(&nodes, "w");
    assert!(!w.is_exported, "nested `w` must be private");
}

#[test]
fn struct_tag_does_not_affect_visibility() {
    // Backtick struct tags (`json:"..."`) are metadata and live in a
    // separate `tag:` child of `field_declaration` — they must not
    // interfere with `name:` capture or the visibility computation.
    let src = "package p\ntype T struct {\n  Name string `json:\"name\"`\n  secret string `json:\"-\"`\n}\n";
    let nodes = parse(src);

    let name = find_property(&nodes, "Name");
    assert!(name.is_exported, "tagged `Name` must still be exported");

    let secret = find_property(&nodes, "secret");
    assert!(!secret.is_exported, "tagged `secret` must still be private");
}
