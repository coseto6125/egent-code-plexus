use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = JavaProvider::new().expect("provider");
    p.parse_file(Path::new("Test.java"), src.as_bytes())
        .expect("parse")
}

fn find_kind(graph: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    graph.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

fn debug_nodes(graph: &LocalGraph) -> Vec<(&str, NodeKind)> {
    graph
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.kind))
        .collect()
}

#[test]
fn java_record_emits_class_node() {
    let src = "public record Point(int x, int y) { public int sum() { return x + y; } }";
    let graph = parse(src);
    assert!(
        find_kind(&graph, "Point", NodeKind::Class),
        "Point must be emitted as Class; got: {:?}",
        debug_nodes(&graph)
    );
}

#[test]
fn java_record_emits_component_properties() {
    let src = "public record Point(int x, int y) { public int sum() { return x + y; } }";
    let graph = parse(src);
    assert!(
        find_kind(&graph, "x", NodeKind::Property),
        "record component `x` must be emitted as Property; got: {:?}",
        debug_nodes(&graph)
    );
    assert!(
        find_kind(&graph, "y", NodeKind::Property),
        "record component `y` must be emitted as Property; got: {:?}",
        debug_nodes(&graph)
    );
}

#[test]
fn java_record_emits_method() {
    let src = "public record Point(int x, int y) { public int sum() { return x + y; } }";
    let graph = parse(src);
    assert!(
        find_kind(&graph, "sum", NodeKind::Method),
        "method `sum` inside record body must still be emitted; got: {:?}",
        debug_nodes(&graph)
    );
}

#[test]
fn java_record_simple_no_body() {
    let src = "record Range(int low, int high) {}";
    let graph = parse(src);
    assert!(
        find_kind(&graph, "Range", NodeKind::Class),
        "Range must be Class; got: {:?}",
        debug_nodes(&graph)
    );
    assert!(
        find_kind(&graph, "low", NodeKind::Property),
        "component `low` must be Property"
    );
    assert!(
        find_kind(&graph, "high", NodeKind::Property),
        "component `high` must be Property"
    );
}

#[test]
fn java_record_annotated() {
    let src = r#"
@JsonDeserialize
public record Person(String name, int age) {}
"#;
    let graph = parse(src);
    assert!(
        find_kind(&graph, "Person", NodeKind::Class),
        "annotated record Person must be Class; got: {:?}",
        debug_nodes(&graph)
    );
    let person_node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Person" && n.kind == NodeKind::Class)
        .expect("Person node");
    assert!(
        !person_node.decorators.is_empty(),
        "Person must carry decorator from @JsonDeserialize"
    );
}

#[test]
fn java_record_heritage() {
    let src = "public record Timestamped(long ts) implements Serializable {}";
    let graph = parse(src);
    assert!(
        find_kind(&graph, "Timestamped", NodeKind::Class),
        "Timestamped must be Class"
    );
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Timestamped" && n.kind == NodeKind::Class)
        .expect("Timestamped node");
    assert!(
        node.heritage.iter().any(|h| h.contains("Serializable")),
        "Timestamped must have Serializable in heritage; got: {:?}",
        node.heritage
    );
}
