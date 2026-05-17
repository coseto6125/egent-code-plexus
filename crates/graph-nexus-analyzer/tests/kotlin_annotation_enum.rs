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
fn plain_class_emits_class_not_enum_not_annotation() {
    let g = parse("class Foo\n");
    let foo = g
        .nodes
        .iter()
        .find(|n| n.name == "Foo")
        .expect("Foo missing");
    assert_eq!(foo.kind, NodeKind::Class, "got {:?}", foo.kind);
}

#[test]
fn enum_class_emits_enum_kind() {
    let g = parse("enum class Color { RED, GREEN }\n");
    let color = g
        .nodes
        .iter()
        .find(|n| n.name == "Color")
        .expect("Color missing");
    assert_eq!(color.kind, NodeKind::Enum, "got {:?}", color.kind);
}

#[test]
fn enum_class_does_not_double_emit_as_class() {
    let g = parse("enum class Color { RED, GREEN }\n");
    let classes: Vec<_> = g.nodes.iter().filter(|n| n.name == "Color").collect();
    assert_eq!(
        classes.len(),
        1,
        "expected exactly one node for Color, got {}",
        classes.len()
    );
    assert_eq!(classes[0].kind, NodeKind::Enum, "got {:?}", classes[0].kind);
}

#[test]
fn annotation_class_emits_annotation_kind() {
    let g = parse("annotation class MyAnno(val x: String)\n");
    let ann = g
        .nodes
        .iter()
        .find(|n| n.name == "MyAnno")
        .expect("MyAnno missing");
    assert_eq!(ann.kind, NodeKind::Annotation, "got {:?}", ann.kind);
}

#[test]
fn annotation_class_does_not_double_emit_as_class() {
    let g = parse("annotation class MyAnno\n");
    let nodes: Vec<_> = g.nodes.iter().filter(|n| n.name == "MyAnno").collect();
    assert_eq!(
        nodes.len(),
        1,
        "expected exactly one node for MyAnno, got {}",
        nodes.len()
    );
    assert_eq!(
        nodes[0].kind,
        NodeKind::Annotation,
        "got {:?}",
        nodes[0].kind
    );
}

#[test]
fn mixed_file_emits_exactly_one_of_each_kind() {
    let src = "class Foo\nenum class Color { RED, GREEN }\nannotation class MyAnno\n";
    let g = parse(src);

    let foo = g
        .nodes
        .iter()
        .find(|n| n.name == "Foo")
        .expect("Foo missing");
    let color = g
        .nodes
        .iter()
        .find(|n| n.name == "Color")
        .expect("Color missing");
    let anno = g
        .nodes
        .iter()
        .find(|n| n.name == "MyAnno")
        .expect("MyAnno missing");

    assert_eq!(foo.kind, NodeKind::Class, "Foo kind: {:?}", foo.kind);
    assert_eq!(color.kind, NodeKind::Enum, "Color kind: {:?}", color.kind);
    assert_eq!(
        anno.kind,
        NodeKind::Annotation,
        "MyAnno kind: {:?}",
        anno.kind
    );

    // Exactly one node per name — no double-emission.
    for name in ["Foo", "Color", "MyAnno"] {
        let count = g.nodes.iter().filter(|n| n.name == name).count();
        assert_eq!(count, 1, "expected 1 node for {}, got {}", name, count);
    }
}
