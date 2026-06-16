//! Swift grammar 0.7.3 syntax coverage — guards the re-vendor that lifted the
//! grammar from 0.7.2. Each construct below parsed to an ERROR node under 0.7.2
//! (dropping the enclosing symbol); 0.7.3 parses them cleanly. The asserts pin
//! "symbol still extracted in the presence of the new syntax" so a future
//! re-vendor that regresses the grammar is caught here.

use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = SwiftProvider::new().expect("provider");
    p.parse_file(Path::new("test.swift"), src.as_bytes())
        .expect("parse")
}

fn func_named<'a>(g: &'a LocalGraph, name: &str) -> &'a ecp_core::analyzer::types::RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name && n.kind == NodeKind::Function)
        .unwrap_or_else(|| panic!("function `{name}` missing in {:#?}", g.nodes))
}

#[test]
fn consuming_parameter_does_not_drop_the_function() {
    let g = parse("func consumeIt(_ x: consuming String) -> String { return x }\n");
    func_named(&g, "consumeIt");
}

#[test]
fn typed_throws_do_catch_does_not_drop_the_function() {
    let g = parse("func mayThrow() throws(MyError) { try risky() }\n");
    func_named(&g, "mayThrow");
}

#[test]
fn nonisolated_unsafe_modifier_keeps_the_property() {
    let g = parse("struct Outer { nonisolated(unsafe) static var shared = 0 }\n");
    let prop = g
        .nodes
        .iter()
        .find(|n| n.name == "shared")
        .unwrap_or_else(|| panic!("property `shared` missing in {:#?}", g.nodes));
    assert_eq!(prop.kind, NodeKind::Property, "got {prop:?}");
}

#[test]
fn nested_type_access_in_return_position_keeps_the_function() {
    let g = parse("func nested() -> Outer.Inner.Value { fatalError() }\n");
    func_named(&g, "nested");
}

#[test]
fn directive_inside_type_body_keeps_the_enclosing_struct() {
    let g = parse("struct Conf {\n#if DEBUG\nvar flag = true\n#endif\n}\n");
    let s = g
        .nodes
        .iter()
        .find(|n| n.name == "Conf" && n.kind == NodeKind::Struct)
        .unwrap_or_else(|| panic!("struct `Conf` missing in {:#?}", g.nodes));
    assert_eq!(s.kind, NodeKind::Struct);
}
