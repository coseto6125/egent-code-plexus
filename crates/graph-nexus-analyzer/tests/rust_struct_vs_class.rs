use graph_nexus_analyzer::rust::parser::RustProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> Vec<(String, NodeKind)> {
    let provider = RustProvider::new().expect("RustProvider::new");
    let graph = provider
        .parse_file(Path::new("test.rs"), source.as_bytes())
        .expect("parse_file");
    graph
        .nodes
        .iter()
        .map(|n| (n.name.clone(), n.kind))
        .collect()
}

#[test]
fn test_struct_emits_struct_not_class() {
    let src = "pub struct Foo { x: u32 }";
    let nodes = parse(src);
    let foo = nodes
        .iter()
        .find(|(n, _)| n == "Foo")
        .expect("Foo not found");
    assert_eq!(
        foo.1,
        NodeKind::Struct,
        "struct Foo must be NodeKind::Struct"
    );
}

#[test]
fn test_enum_emits_enum_not_class() {
    let src = "pub enum Color { Red, Green, Blue }";
    let nodes = parse(src);
    let color = nodes
        .iter()
        .find(|(n, _)| n == "Color")
        .expect("Color not found");
    assert_eq!(color.1, NodeKind::Enum, "enum Color must be NodeKind::Enum");
}

#[test]
fn test_trait_emits_trait_not_interface() {
    let src = "pub trait Animal { fn speak(&self); }";
    let nodes = parse(src);
    let animal = nodes
        .iter()
        .find(|(n, _)| n == "Animal")
        .expect("Animal not found");
    assert_eq!(
        animal.1,
        NodeKind::Trait,
        "trait Animal must be NodeKind::Trait"
    );
}

#[test]
fn test_struct_and_enum_coexist() {
    let src = r#"
pub struct Point { x: f64, y: f64 }
pub enum Shape { Circle(f64), Square(f64) }
pub fn area(s: Shape) -> f64 { 0.0 }
"#;
    let nodes = parse(src);
    let point = nodes.iter().find(|(n, _)| n == "Point").expect("Point");
    assert_eq!(point.1, NodeKind::Struct);
    let shape = nodes.iter().find(|(n, _)| n == "Shape").expect("Shape");
    assert_eq!(shape.1, NodeKind::Enum);
    let area = nodes.iter().find(|(n, _)| n == "area").expect("area");
    assert_eq!(area.1, NodeKind::Function);
}

#[test]
fn test_no_class_emitted_for_rust_types() {
    let src = r#"
struct A;
enum B { X }
trait C {}
"#;
    let nodes = parse(src);
    assert!(
        nodes.iter().all(|(_, k)| *k != NodeKind::Class),
        "NodeKind::Class must not appear for Rust struct/enum/trait: {nodes:?}"
    );
    assert!(
        nodes.iter().all(|(_, k)| *k != NodeKind::Interface),
        "NodeKind::Interface must not appear for Rust trait: {nodes:?}"
    );
}
