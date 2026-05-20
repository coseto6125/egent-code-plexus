//! SystemVerilog Named dimension: `typedef` declarations emit `NodeKind::Typedef`.
//!
//! `typedef logic [7:0] byte_t;` → alias name `byte_t`.
//! Captures via `(type_declaration (simple_identifier) @typedef.name)`.

use ecp_analyzer::verilog::parser::VerilogProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = VerilogProvider::new().expect("VerilogProvider init");
    let graph = provider
        .parse_file(Path::new("test.sv"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find_typedef<'a>(nodes: &'a [RawNode], name: &str) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == NodeKind::Typedef)
        .unwrap_or_else(|| panic!("missing Typedef node `{name}` in {nodes:#?}"))
}

#[test]
fn test_sv_logic_typedef_emits_typedef() {
    let nodes = parse("typedef logic [7:0] byte_t;");
    find_typedef(&nodes, "byte_t");
}

#[test]
fn test_sv_int_typedef_emits_typedef() {
    let nodes = parse("typedef int my_int_t;");
    find_typedef(&nodes, "my_int_t");
}

#[test]
fn test_sv_enum_typedef_emits_typedef() {
    let nodes = parse("typedef enum { RED, GREEN, BLUE } color_t;");
    find_typedef(&nodes, "color_t");
}

#[test]
fn test_sv_typedef_does_not_suppress_module() {
    // A module declaration alongside a typedef — both must be emitted.
    let src = "module top; endmodule\ntypedef int word_t;";
    let nodes = parse(src);
    find_typedef(&nodes, "word_t");
    nodes
        .iter()
        .find(|n| n.name == "top" && n.kind == NodeKind::Class)
        .expect("module `top` must still emit as Class");
}

#[test]
fn test_sv_multiple_typedefs() {
    let src = "typedef logic [7:0] byte_t;\ntypedef logic [15:0] word_t;";
    let nodes = parse(src);
    find_typedef(&nodes, "byte_t");
    find_typedef(&nodes, "word_t");
}
