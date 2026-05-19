use graph_nexus_analyzer::kotlin::parser::KotlinProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = KotlinProvider::new().expect("provider");
    p.parse_file(Path::new("Test.kt"), src.as_bytes())
        .expect("parse")
}

#[test]
fn class_fun_emits_method_kind() {
    let g = parse("class Foo {\n    fun bar() {}\n}\n");
    let bar = g.nodes.iter().find(|n| n.name == "bar").expect("bar missing");
    assert_eq!(bar.kind, NodeKind::Method, "got {:?}", bar);
}

#[test]
fn top_level_fun_stays_function() {
    let g = parse("fun topLevel() {}\n");
    let top = g
        .nodes
        .iter()
        .find(|n| n.name == "topLevel")
        .expect("topLevel missing");
    assert_eq!(top.kind, NodeKind::Function, "got {:?}", top);
}
