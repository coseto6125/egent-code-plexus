use graph_nexus_analyzer::c::parser::CProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let provider = CProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.c"), src.as_bytes())
        .expect("parse")
}

#[test]
fn test_c_typedef_primitive_emits_typedef_node() {
    let graph = parse("typedef int Foo;\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Foo")
        .unwrap_or_else(|| panic!("expected Typedef node `Foo`, got {:#?}", graph.nodes));
    assert_eq!(node.kind, NodeKind::Typedef);
}

#[test]
fn test_c_typedef_struct_emits_struct_and_typedef() {
    let graph = parse("typedef struct Bar { int x; } Bar;\n");
    let struct_node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Bar" && n.kind == NodeKind::Struct);
    let typedef_node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Bar" && n.kind == NodeKind::Typedef);
    assert!(
        struct_node.is_some(),
        "expected Struct node `Bar`, got {:#?}",
        graph.nodes
    );
    assert!(
        typedef_node.is_some(),
        "expected Typedef node `Bar`, got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_plain_struct_emits_struct_node() {
    let graph = parse("struct Baz { int y; };\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Baz")
        .unwrap_or_else(|| panic!("expected Struct node `Baz`, got {:#?}", graph.nodes));
    assert_eq!(node.kind, NodeKind::Struct);
}

#[test]
fn test_c_enum_emits_enum_node() {
    let graph = parse("enum E { A, B };\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "E")
        .unwrap_or_else(|| panic!("expected Enum node `E`, got {:#?}", graph.nodes));
    assert_eq!(node.kind, NodeKind::Enum);
}
