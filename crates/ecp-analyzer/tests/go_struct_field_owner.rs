//! Regression tests for Go struct-field `owner_class` population.
//!
//! Two structs with the same field name used to emit `Property` nodes with
//! `owner_class: None`, producing identical uids and colliding as BlindSpots.
//! After the fix, `owner_class` is set to the enclosing named struct type
//! so `FooBarFileStruct::File` and `FooBarFileFailStruct::File` get distinct
//! uids.

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse_properties(src: &str) -> Vec<(String, Option<String>)> {
    let provider = GoProvider::new().expect("GoProvider init");
    let graph = provider
        .parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse_file");
    graph
        .nodes
        .into_iter()
        .filter(|n| n.kind == NodeKind::Property)
        .map(|n| (n.name, n.owner_class))
        .collect()
}

#[test]
fn single_struct_field_has_owner_class() {
    let src = "package p\ntype User struct {\n  Name string\n}\n";
    let props = parse_properties(src);
    let (_, owner) = props
        .iter()
        .find(|(n, _)| n == "Name")
        .expect("Name missing");
    assert_eq!(
        owner.as_deref(),
        Some("User"),
        "expected owner_class=User, got {owner:?}"
    );
}

#[test]
fn two_structs_same_field_name_distinct_owners() {
    // This is the binding_test.go pattern: FooBarFileStruct and
    // FooBarFileFailStruct both declare `File *multipart.FileHeader`.
    // Before the fix both emitted owner_class=None → uid collision.
    let src = r#"package p
import "mime/multipart"
type FooBarFileStruct struct {
    File *multipart.FileHeader
}
type FooBarFileFailStruct struct {
    File *multipart.FileHeader
}
"#;
    let props = parse_properties(src);
    let file_props: Vec<_> = props.iter().filter(|(n, _)| n == "File").collect();
    assert_eq!(
        file_props.len(),
        2,
        "expected two Property nodes named File, got {file_props:#?}"
    );
    let owners: std::collections::HashSet<Option<&str>> =
        file_props.iter().map(|(_, o)| o.as_deref()).collect();
    assert!(
        owners.contains(&Some("FooBarFileStruct")),
        "missing owner FooBarFileStruct in {owners:?}"
    );
    assert!(
        owners.contains(&Some("FooBarFileFailStruct")),
        "missing owner FooBarFileFailStruct in {owners:?}"
    );
}

#[test]
fn three_structs_same_field_all_distinct() {
    let src = r#"package p
type A struct { X int }
type B struct { X string }
type C struct { X bool }
"#;
    let props = parse_properties(src);
    let x_props: Vec<_> = props.iter().filter(|(n, _)| n == "X").collect();
    assert_eq!(
        x_props.len(),
        3,
        "expected 3 Property X nodes, got {x_props:#?}"
    );
    let owners: Vec<Option<&str>> = x_props.iter().map(|(_, o)| o.as_deref()).collect();
    assert!(owners.contains(&Some("A")));
    assert!(owners.contains(&Some("B")));
    assert!(owners.contains(&Some("C")));
}

#[test]
fn anonymous_inline_struct_field_has_no_owner_class() {
    // Anonymous inline struct fields have no enclosing named type_spec.
    // They should remain owner_class=None — we don't manufacture a fake name.
    let src = "package p\ntype Outer struct {\n  Inner struct {\n    Z string\n  }\n}\n";
    let props = parse_properties(src);
    let z = props.iter().find(|(n, _)| n == "Z").expect("Z missing");
    // Z is inside an anonymous struct; no named type_spec ancestor → None.
    assert_eq!(
        z.1.as_deref(),
        None,
        "anonymous-struct field should have owner_class=None, got {:?}",
        z.1
    );
    // But the outer `Inner` field is directly in `Outer` → owner=Outer.
    let inner = props
        .iter()
        .find(|(n, _)| n == "Inner")
        .expect("Inner missing");
    assert_eq!(
        inner.1.as_deref(),
        Some("Outer"),
        "outer field Inner should have owner_class=Outer, got {:?}",
        inner.1
    );
}
