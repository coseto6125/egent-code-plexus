//! Protocol property requirements — `var name: String { get }` inside a
//! `protocol` body must emit Property nodes (filter-B node coverage).
//!
//! tree-sitter-swift uses `protocol_property_declaration` (distinct from
//! `property_declaration`) for property requirements. Without the dedicated
//! capture in queries.scm, protocol property contracts were invisible to the
//! graph while method requirements were fully captured.

use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<ecp_core::analyzer::types::RawNode> {
    let provider = SwiftProvider::new().expect("SwiftProvider init");
    provider
        .parse_file(Path::new("t.swift"), src.as_bytes())
        .expect("parse_file")
        .nodes
}

fn find<'a>(
    nodes: &'a [ecp_core::analyzer::types::RawNode],
    name: &str,
    kind: NodeKind,
) -> &'a ecp_core::analyzer::types::RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

const PROTOCOL_SRC: &str = r#"protocol Named {
    var name: String { get }
    var age: Int { get set }
    func greet() -> String
}"#;

#[test]
fn protocol_property_requirement_name_emitted() {
    let nodes = parse(PROTOCOL_SRC);
    find(&nodes, "name", NodeKind::Property);
}

#[test]
fn protocol_property_requirement_age_emitted() {
    let nodes = parse(PROTOCOL_SRC);
    find(&nodes, "age", NodeKind::Property);
}

#[test]
fn protocol_function_requirement_not_regressed() {
    // Ensure the existing protocol_function_declaration capture still fires.
    let nodes = parse(PROTOCOL_SRC);
    // greet is inside protocol_body → is_class_method returns true → Method
    find(&nodes, "greet", NodeKind::Method);
}

#[test]
fn protocol_property_type_annotation_captured() {
    // The type annotation `: String` should surface on the emitted Property.
    let nodes = parse(PROTOCOL_SRC);
    let name_node = find(&nodes, "name", NodeKind::Property);
    assert_eq!(
        name_node.type_annotation.as_deref(),
        Some("String"),
        "expected type_annotation=String, got {:?}",
        name_node.type_annotation
    );
}

#[test]
fn protocol_property_age_type_annotation_captured() {
    let nodes = parse(PROTOCOL_SRC);
    let age_node = find(&nodes, "age", NodeKind::Property);
    assert_eq!(
        age_node.type_annotation.as_deref(),
        Some("Int"),
        "expected type_annotation=Int, got {:?}",
        age_node.type_annotation
    );
}
