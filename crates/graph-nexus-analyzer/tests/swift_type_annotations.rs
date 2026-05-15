//! Type annotations on Swift nodes (parameters, properties, vars).
//!
//! Swift uses postfix type syntax `name: Type` (vs C's prefix `Type name`),
//! so the parser reads the type-annotation node text directly rather than
//! slicing source-before-the-name. Ported from upstream
//! `_source_code/gitnexus/src/core/ingestion/type-extractors/swift.ts`.

use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = SwiftProvider::new().expect("SwiftProvider init");
    let graph = provider
        .parse_file(Path::new("t.swift"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

#[test]
fn param_simple_type() {
    let nodes = parse("func f(name: String) {}");
    let p = find(&nodes, "name", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("String"));
}

#[test]
fn param_int_type() {
    let nodes = parse("func f(count: Int) {}");
    let p = find(&nodes, "count", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("Int"));
}

#[test]
fn multiple_params_all_typed() {
    let nodes = parse("func f(name: String, age: Int) {}");
    assert_eq!(
        find(&nodes, "name", NodeKind::Variable)
            .type_annotation
            .as_deref(),
        Some("String")
    );
    assert_eq!(
        find(&nodes, "age", NodeKind::Variable)
            .type_annotation
            .as_deref(),
        Some("Int")
    );
}

#[test]
fn class_var_property_type() {
    let nodes = parse(
        r#"class C {
            var counter: Int = 0
        }"#,
    );
    let p = find(&nodes, "counter", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("Int"));
}

#[test]
fn class_let_property_type() {
    let nodes = parse(
        r#"class C {
            let name: String
        }"#,
    );
    let p = find(&nodes, "name", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("String"));
}

#[test]
fn top_level_let_with_type() {
    let nodes = parse("let pi: Double = 3.14\n");
    let p = find(&nodes, "pi", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("Double"));
}
