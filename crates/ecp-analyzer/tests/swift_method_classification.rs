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

#[test]
fn class_func_emits_method_kind() {
    let g = parse("class Foo {\n    func bar() {}\n}\n");
    let bar = g
        .nodes
        .iter()
        .find(|n| n.name == "bar")
        .expect("bar missing");
    assert_eq!(bar.kind, NodeKind::Method, "got {:?}", bar);
}

#[test]
fn struct_func_emits_method_kind() {
    let g = parse("struct Baz {\n    func qux() -> Int { return 1 }\n}\n");
    let qux = g
        .nodes
        .iter()
        .find(|n| n.name == "qux")
        .expect("qux missing");
    assert_eq!(qux.kind, NodeKind::Method, "got {:?}", qux);
}

#[test]
fn top_level_func_stays_function() {
    let g = parse("func topLevel() {}\n");
    let top = g
        .nodes
        .iter()
        .find(|n| n.name == "topLevel")
        .expect("topLevel missing");
    assert_eq!(top.kind, NodeKind::Function, "got {:?}", top);
}

#[test]
fn enum_method_emits_method_kind() {
    let g = parse("enum E {\n    case a\n    func describe() -> String { return \"a\" }\n}\n");
    let m = g
        .nodes
        .iter()
        .find(|n| n.name == "describe")
        .expect("describe missing");
    assert_eq!(m.kind, NodeKind::Method, "got {:?}", m);
}

#[test]
fn protocol_method_requirement_emits_method_kind() {
    // tree-sitter-swift uses `protocol_function_declaration` (distinct from
    // `function_declaration`) for protocol body methods. Without the dedicated
    // query rule in queries.scm, the 11 Alamofire `Source/Core/*.swift` /
    // `Source/Features/*.swift` protocol requirements surfaced as ref_over
    // Method-* rows in the 2026-05-19 parity report.
    let g = parse(
        "protocol DataDecoder {\n    \
         func decode<D: Decodable>(_ type: D.Type, from data: Data) throws -> D\n\
         }\n",
    );
    let m = g
        .nodes
        .iter()
        .find(|n| n.name == "decode")
        .expect("decode missing");
    assert_eq!(m.kind, NodeKind::Method, "got {:?}", m);
}

#[test]
fn protocol_method_with_inout_param_emits_method_kind() {
    // AuthenticationInterceptor.swift `apply(_:to:)` shape — `inout` parameter
    // is a Swift-specific function parameter modifier that tree-sitter-swift
    // 0.25 parses identically inside `protocol_function_declaration`.
    let g = parse(
        "protocol RequestInterceptor {\n    \
         func apply(_ credential: Credential, to urlRequest: inout URLRequest)\n\
         }\n",
    );
    let m = g
        .nodes
        .iter()
        .find(|n| n.name == "apply")
        .expect("apply missing");
    assert_eq!(m.kind, NodeKind::Method, "got {:?}", m);
}
