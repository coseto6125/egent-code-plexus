//! Rust enum variant struct-form fields emit as Property — parity with
//! ref-gitnexus and with the struct-field rule.
//!
//! `enum E { V { f1: T } }` — `f1` is a permanent type-level data member;
//! pattern-match destructuring `V { f1 } => ...` references it by name, so
//! it must be discoverable as Property.

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = RustProvider::new().expect("provider");
    p.parse_file(Path::new("lib.rs"), src.as_bytes())
        .expect("parse")
}

fn properties(g: &LocalGraph) -> Vec<&str> {
    let mut v: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property)
        .map(|n| n.name.as_str())
        .collect();
    v.sort();
    v
}

#[test]
fn single_variant_field_emits_property() {
    let g = parse("enum E { V { f1: i32 } }");
    assert_eq!(properties(&g), vec!["f1"]);
}

#[test]
fn multiple_variant_fields_each_emit() {
    let g = parse("enum Request { Get { key: String }, Set { key: String, value: String } }");
    assert_eq!(properties(&g), vec!["key", "key", "value"]);
}

#[test]
fn variant_field_emits_once_no_dup() {
    let g = parse("enum E { V { x: i32 } }");
    let p = properties(&g);
    assert_eq!(p.len(), 1, "expected single Property, got {:?}", g.nodes);
}

#[test]
fn tuple_variant_emits_no_property() {
    let g = parse("enum E { V(i32, String) }");
    assert!(
        properties(&g).is_empty(),
        "tuple variants have no field_identifier, got {:?}",
        g.nodes
    );
}

#[test]
fn unit_variant_emits_no_property() {
    let g = parse("enum E { V }");
    assert!(properties(&g).is_empty());
}

#[test]
fn mixed_struct_and_enum_variant_fields() {
    let g = parse("struct S { a: i32 } enum E { V { b: i32 } }");
    assert_eq!(properties(&g), vec!["a", "b"]);
}
