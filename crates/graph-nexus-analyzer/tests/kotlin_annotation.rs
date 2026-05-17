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
fn annotation_class_emits_annotation_kind() {
    let g = parse("annotation class MyAnn\n");
    let ann = g
        .nodes
        .iter()
        .find(|n| n.name == "MyAnn")
        .expect("MyAnn missing");
    assert_eq!(ann.kind, NodeKind::Annotation, "got {:?}", ann);
}

#[test]
fn annotation_class_with_params_emits_annotation_kind() {
    let g = parse("annotation class Repeated(val v: String)\n");
    let ann = g
        .nodes
        .iter()
        .find(|n| n.name == "Repeated")
        .expect("Repeated missing");
    assert_eq!(ann.kind, NodeKind::Annotation, "got {:?}", ann);
}

#[test]
fn plain_class_still_emits_class_kind() {
    let g = parse("class Foo\n");
    let foo = g
        .nodes
        .iter()
        .find(|n| n.name == "Foo")
        .expect("Foo missing");
    assert_eq!(foo.kind, NodeKind::Class, "got {:?}", foo);
}
