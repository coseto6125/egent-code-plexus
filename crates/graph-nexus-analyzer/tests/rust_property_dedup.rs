//! Regression: rust/queries.scm had two byte-identical `@property.name`
//! capture patterns for struct fields, causing every Property to emit
//! twice. Final_baseline showed Rust Property rs=2624 vs ref=1314 (+1310
//! over) — almost exactly double.

use graph_nexus_analyzer::rust::parser::RustProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = RustProvider::new().expect("provider");
    p.parse_file(Path::new("lib.rs"), src.as_bytes())
        .expect("parse")
}

fn properties<'a>(g: &'a LocalGraph) -> Vec<&'a str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn single_struct_field_emits_once() {
    let g = parse("struct Foo { x: i32 }");
    let p = properties(&g);
    assert_eq!(p, vec!["x"], "expected single Property, got {:?}", g.nodes);
}

#[test]
fn three_fields_emit_three_properties() {
    let g = parse("struct Foo { a: i32, b: i32, c: i32 }");
    let p = properties(&g);
    assert_eq!(
        p,
        vec!["a", "b", "c"],
        "expected exactly 3 distinct Properties, got {:?}",
        g.nodes
    );
}

#[test]
fn pub_field_still_emits_once() {
    let g = parse("pub struct Foo { pub x: i32 }");
    let p = properties(&g);
    assert_eq!(
        p,
        vec!["x"],
        "pub modifier should not duplicate Property emission"
    );
}

#[test]
fn multiple_structs_no_cross_duplication() {
    let g = parse("struct A { x: i32 } struct B { x: i32 }");
    let p = properties(&g);
    assert_eq!(
        p.len(),
        2,
        "expected one Property per struct, got {:?}",
        g.nodes
    );
}
