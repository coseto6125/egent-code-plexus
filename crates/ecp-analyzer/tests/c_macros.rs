use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let provider = CProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.c"), src.as_bytes())
        .expect("parse")
}

#[test]
fn test_c_object_macro_emits_macro_node() {
    let graph = parse("#define VERSION \"1.0\"\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "VERSION")
        .unwrap_or_else(|| panic!("expected Macro node `VERSION`, got {:#?}", graph.nodes));
    assert_eq!(node.kind, NodeKind::Macro);
}

#[test]
fn test_c_function_macro_emits_macro_node() {
    let graph = parse("#define ADD(a,b) ((a)+(b))\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "ADD")
        .unwrap_or_else(|| panic!("expected Macro node `ADD`, got {:#?}", graph.nodes));
    assert_eq!(node.kind, NodeKind::Macro);
}
