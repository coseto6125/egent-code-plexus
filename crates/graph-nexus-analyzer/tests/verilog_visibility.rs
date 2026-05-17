//! SystemVerilog class-member visibility checks for the Verilog provider.
//!
//! In SystemVerilog (IEEE 1800), class members are implicitly public unless
//! marked `local` or `protected`.  tree-sitter-verilog 1.0.3 exposes this
//! via `class_item_qualifier` child nodes inside `class_property`.
//!
//! * No qualifier       → is_exported = true
//! * `local` qualifier  → is_exported = false
//! * `protected` qualifier → is_exported = false

use graph_nexus_analyzer::verilog::parser::VerilogProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = VerilogProvider::new().expect("VerilogProvider init");
    let graph = provider
        .parse_file(Path::new("test.sv"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn public_class_property_is_exported() {
    let src = "class C; int y; endclass";
    let nodes = parse(src);
    let y = find(&nodes, "y", NodeKind::Property);
    assert!(y.is_exported, "`y` (no qualifier) must be exported");
}

#[test]
fn local_class_property_is_not_exported() {
    let src = "class C; local int x; endclass";
    let nodes = parse(src);
    let x = find(&nodes, "x", NodeKind::Property);
    assert!(!x.is_exported, "`x` (local) must not be exported");
}

#[test]
fn protected_class_property_is_not_exported() {
    let src = "class C; protected int z; endclass";
    let nodes = parse(src);
    let z = find(&nodes, "z", NodeKind::Property);
    assert!(!z.is_exported, "`z` (protected) must not be exported");
}

#[test]
fn mixed_class_properties() {
    let src = "class C; local int x; int y; endclass";
    let nodes = parse(src);

    let x = find(&nodes, "x", NodeKind::Property);
    assert!(!x.is_exported, "`x` (local) must not be exported");

    let y = find(&nodes, "y", NodeKind::Property);
    assert!(y.is_exported, "`y` (no qualifier) must be exported");
}
