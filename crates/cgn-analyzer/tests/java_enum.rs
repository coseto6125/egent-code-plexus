use cgn_analyzer::java::parser::JavaProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = JavaProvider::new().expect("provider");
    p.parse_file(Path::new("Test.java"), src.as_bytes())
        .expect("parse")
}

fn find_kind(graph: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| n.name == name && n.kind == kind)
}

#[test]
fn java_enum_simple() {
    let graph = parse("enum Color { RED, BLUE }");
    assert!(
        find_kind(&graph, "Color", NodeKind::Enum),
        "Color must be Enum; got: {:?}",
        graph.nodes.iter().map(|n| (&n.name, &n.kind)).collect::<Vec<_>>()
    );
}

#[test]
fn java_enum_public() {
    let graph = parse("public enum Status { ACTIVE, INACTIVE }");
    assert!(
        find_kind(&graph, "Status", NodeKind::Enum),
        "Status must be Enum"
    );
}

#[test]
fn java_enum_with_constructor_and_method() {
    let src = r#"
enum Planet {
    MERCURY(3.303e+23, 2.4397e6),
    VENUS  (4.869e+24, 6.0518e6);

    private final double mass;
    private final double radius;

    Planet(double mass, double radius) {
        this.mass = mass;
        this.radius = radius;
    }

    double surfaceGravity() {
        final double G = 6.67300E-11;
        return G * mass / (radius * radius);
    }
}
"#;
    let graph = parse(src);
    assert!(
        find_kind(&graph, "Planet", NodeKind::Enum),
        "Planet must be Enum"
    );
    // Constructor and method inside enum still emit their own nodes.
    assert!(
        find_kind(&graph, "Planet", NodeKind::Constructor)
            || graph.nodes.iter().any(|n| n.kind == NodeKind::Constructor),
        "enum constructor should still be emitted"
    );
    assert!(
        find_kind(&graph, "surfaceGravity", NodeKind::Method),
        "surfaceGravity method must be emitted"
    );
}
