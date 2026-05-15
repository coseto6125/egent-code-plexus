//! Type annotations on C nodes (params, fields, return types, top-level vars).
//!
//! Covers Wave 2 task D3 from
//! `docs/specs/2026-05-15-language-coverage-gaps.md`.
//!
//! Conventions documented in `src/c/parser.rs::slice_type_before`:
//! - **Pointer spacing**: preserved as-written. `char* s` → `"char*"`,
//!   `char * s` → `"char *"`. Source is source of truth.
//! - **Qualifier inclusion**: full prefix including storage-class. E.g.
//!   `static const int N` → `"static const int"`.

use graph_nexus_analyzer::c::parser::CProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CProvider::new().expect("CProvider init");
    let graph = provider
        .parse_file(Path::new("t.c"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

#[test]
fn param_primitive() {
    let nodes = parse("void f(int x);\n");
    let p = find(&nodes, "x", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("int"));
}

#[test]
fn param_pointer() {
    // Source spelling `char* s` is preserved (no space before `*`).
    let nodes = parse("void f(char* s);\n");
    let p = find(&nodes, "s", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("char*"));
}

#[test]
fn param_const_pointer() {
    let nodes = parse("void f(const char* s);\n");
    let p = find(&nodes, "s", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("const char*"));
}

#[test]
fn return_type_int() {
    let nodes = parse("int f() { return 0; }\n");
    let f = find(&nodes, "f", NodeKind::Function);
    assert_eq!(f.type_annotation.as_deref(), Some("int"));
}

#[test]
fn return_type_void() {
    let nodes = parse("void f() {}\n");
    let f = find(&nodes, "f", NodeKind::Function);
    assert_eq!(f.type_annotation.as_deref(), Some("void"));
}

#[test]
fn struct_field() {
    let nodes = parse("struct S { int x; };\n");
    let x = find(&nodes, "x", NodeKind::Property);
    assert_eq!(x.type_annotation.as_deref(), Some("int"));
}

#[test]
fn struct_field_pointer() {
    let nodes = parse("struct S { char* name; };\n");
    let n = find(&nodes, "name", NodeKind::Property);
    assert_eq!(n.type_annotation.as_deref(), Some("char*"));
}

#[test]
fn var_with_qualifiers() {
    // Documented convention: the full declaration prefix is kept, so
    // `static const int N = 5;` → `"static const int"`. Consumers wanting
    // only the bare type can strip storage-class words downstream.
    let nodes = parse("static const int N = 5;\n");
    let v = find(&nodes, "N", NodeKind::Variable);
    let annot = v.type_annotation.as_deref().unwrap_or("");
    assert!(
        annot.contains("int"),
        "expected `int` in annotation, got {annot:?}"
    );
    assert!(
        annot.contains("static") && annot.contains("const"),
        "expected qualifier prefix preserved, got {annot:?}"
    );
}
