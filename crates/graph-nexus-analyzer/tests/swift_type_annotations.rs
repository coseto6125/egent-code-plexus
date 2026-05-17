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

// param_* tests removed: formal parameters are no longer emitted as
// Variable nodes (see `fix(analyzer): drop formal_parameter Variable
// emission ...`).

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

#[test]
fn class_var_untyped_emits_property() {
    let nodes = parse("class C { var x = 0 }\n");
    find(&nodes, "x", NodeKind::Property);
}

#[test]
fn class_optional_type_property() {
    let nodes = parse("class C { var x: Int? }\n");
    let p = find(&nodes, "x", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("Int?"));
}

#[test]
fn class_array_type_property() {
    let nodes = parse("class C { var items: [String] }\n");
    let p = find(&nodes, "items", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("[String]"));
}

#[test]
fn class_closure_type_property() {
    let nodes = parse("class C { var handler: (Int) -> Bool }\n");
    let p = find(&nodes, "handler", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("(Int) -> Bool"));
}

#[test]
fn tuple_pattern_emits_both_names() {
    let nodes = parse("class C { let (a, b) = (1, 2) }\n");
    find(&nodes, "a", NodeKind::Property);
    find(&nodes, "b", NodeKind::Property);
}

#[test]
fn init_emits_constructor() {
    let nodes = parse("class C { init() {} }\n");
    find(&nodes, "init", NodeKind::Constructor);
}

#[test]
fn init_with_params_emits_constructor() {
    let nodes = parse("class C { init(x: Int, y: String) {} }\n");
    find(&nodes, "init", NodeKind::Constructor);
}
