use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = GoProvider::new().expect("provider");
    p.parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse")
}

#[test]
fn type_struct_emits_struct_kind() {
    let g = parse("package p\n\ntype Foo struct {\n    X int\n    Y string\n}\n");
    let foo = g
        .nodes
        .iter()
        .find(|n| n.name == "Foo")
        .expect("Foo missing");
    assert_eq!(foo.kind, NodeKind::Struct, "got {:?}", foo);
}

#[test]
fn type_interface_still_emits_interface_kind() {
    let g = parse("package p\n\ntype Stringer interface {\n    String() string\n}\n");
    let s = g
        .nodes
        .iter()
        .find(|n| n.name == "Stringer")
        .expect("Stringer missing");
    assert_eq!(s.kind, NodeKind::Interface, "got {:?}", s);
}
