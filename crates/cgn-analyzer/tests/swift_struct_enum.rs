use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = SwiftProvider::new().expect("provider");
    p.parse_file(Path::new("test.swift"), src.as_bytes()).expect("parse")
}

#[test]
fn struct_emits_struct_kind() {
    let g = parse("struct Foo { let x: Int = 1 }\n");
    let foo = g.nodes.iter().find(|n| n.name == "Foo").expect("Foo missing");
    assert_eq!(foo.kind, NodeKind::Struct, "got {:?}", foo);
}

#[test]
fn enum_emits_enum_kind() {
    let g = parse("enum Color { case red, blue }\n");
    let color = g.nodes.iter().find(|n| n.name == "Color").expect("Color missing");
    assert_eq!(color.kind, NodeKind::Enum, "got {:?}", color);
}

#[test]
fn class_still_emits_class_kind() {
    let g = parse("class Bar { let y: String = \"x\" }\n");
    let bar = g.nodes.iter().find(|n| n.name == "Bar").expect("Bar missing");
    assert_eq!(bar.kind, NodeKind::Class, "got {:?}", bar);
}

#[test]
fn protocol_emits_trait_kind() {
    let g = parse("protocol Greetable { func greet() -> String }\n");
    let p = g.nodes.iter().find(|n| n.name == "Greetable").expect("Greetable missing");
    assert_eq!(p.kind, NodeKind::Trait, "got {:?}", p);
}
