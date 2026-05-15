//! Type annotations on Dart nodes (parameters, properties, top-level vars,
//! return types).
//!
//! Dart uses prefix type syntax (`int x`, `String name`); tree-sitter-dart
//! exposes the `(type ...)` node as a sibling of the identifier rather than
//! a `name:`-prefixed field, so the parser descends positionally. Ported
//! from upstream `_source_code/gitnexus/src/core/ingestion/type-extractors/dart.ts`.

use graph_nexus_analyzer::dart::parser::DartProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = DartProvider::new().expect("DartProvider init");
    let graph = provider
        .parse_file(Path::new("t.dart"), src.as_bytes())
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
fn param_primitive_type() {
    let nodes = parse("bool f(int age) => true;\n");
    let p = find(&nodes, "age", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("int"));
}

#[test]
fn param_object_type() {
    let nodes = parse("bool f(String name) => true;\n");
    let p = find(&nodes, "name", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("String"));
}

#[test]
fn multiple_params_all_typed() {
    let nodes = parse("bool f(String name, int age) => true;\n");
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
        Some("int")
    );
}

#[test]
fn class_field_int() {
    let nodes = parse(
        r#"class C {
  int counter = 0;
}
"#,
    );
    let p = find(&nodes, "counter", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("int"));
}

#[test]
fn class_field_string() {
    let nodes = parse(
        r#"class C {
  String name = '';
}
"#,
    );
    let p = find(&nodes, "name", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("String"));
}

#[test]
fn top_level_variable_with_type() {
    let nodes = parse("double pi = 3.14;\n");
    let v = find(&nodes, "pi", NodeKind::Variable);
    assert_eq!(v.type_annotation.as_deref(), Some("double"));
}

#[test]
fn function_return_type_preserved() {
    // Return type was already captured pre-Wave 3 — assert it still works.
    let nodes = parse("bool f() => true;\n");
    let f = find(&nodes, "f", NodeKind::Function);
    assert_eq!(f.type_annotation.as_deref(), Some("bool"));
}
