use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = SwiftProvider::new().expect("provider");
    p.parse_file(Path::new("test.swift"), src.as_bytes())
        .expect("parse")
}

#[test]
fn class_func_emits_method_kind() {
    let g = parse("class Foo {\n    func bar() {}\n}\n");
    let bar = g
        .nodes
        .iter()
        .find(|n| n.name == "bar")
        .expect("bar missing");
    assert_eq!(bar.kind, NodeKind::Method, "got {:?}", bar);
}

#[test]
fn struct_func_emits_method_kind() {
    let g = parse("struct Baz {\n    func qux() -> Int { return 1 }\n}\n");
    let qux = g
        .nodes
        .iter()
        .find(|n| n.name == "qux")
        .expect("qux missing");
    assert_eq!(qux.kind, NodeKind::Method, "got {:?}", qux);
}

#[test]
fn top_level_func_stays_function() {
    let g = parse("func topLevel() {}\n");
    let top = g
        .nodes
        .iter()
        .find(|n| n.name == "topLevel")
        .expect("topLevel missing");
    assert_eq!(top.kind, NodeKind::Function, "got {:?}", top);
}

#[test]
fn enum_method_emits_method_kind() {
    let g = parse("enum E {\n    case a\n    func describe() -> String { return \"a\" }\n}\n");
    let m = g
        .nodes
        .iter()
        .find(|n| n.name == "describe")
        .expect("describe missing");
    assert_eq!(m.kind, NodeKind::Method, "got {:?}", m);
}
