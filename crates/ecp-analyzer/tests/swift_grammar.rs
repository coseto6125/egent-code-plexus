//! Swift grammar upgrade guards.
//!
//! 0.7.3: each construct in the first group parsed to an ERROR node under 0.7.2
//! (dropping the enclosing symbol); 0.7.3 parses them cleanly. The asserts pin
//! "symbol still extracted in the presence of the new syntax".
//!
//! 0.7.4 binds a prefix operator to the callee (`!a.m(x)` parses as
//! `(!a.m)(x)`) and a continued binary operator to the next operand
//! (`a +\n B.m(x)` parses as `(a + B.m)(x)`). The second group pins that the
//! call still names `m`, as it did under 0.7.3.

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

fn calls_of(g: &LocalGraph, name: &str) -> Vec<String> {
    g.nodes
        .iter()
        .find(|n| n.name == name && matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .unwrap_or_else(|| panic!("function `{name}` missing in {:#?}", g.nodes))
        .calls
        .clone()
}

fn calls_member(calls: &[String], member: &str) -> bool {
    calls
        .iter()
        .any(|c| c == member || c.ends_with(&format!(".{member}")))
}

#[test]
fn test_swift_callee_negated_member_call_in_macro_names_method() {
    let g = parse("func check() {\n    #expect(!appJSON.matches(textPlain))\n}\n");
    let calls = calls_of(&g, "check");
    assert!(calls_member(&calls, "matches"), "calls: {calls:?}");
}

#[test]
fn test_swift_callee_negated_member_call_in_guard_names_method() {
    let g = parse("func check() {\n    guard !split.contains(name) else { return }\n}\n");
    let calls = calls_of(&g, "check");
    assert!(calls_member(&calls, "contains"), "calls: {calls:?}");
}

#[test]
fn test_swift_callee_negated_bare_call_names_function() {
    let g = parse("func check() {\n    if !isReady() { return }\n}\n");
    let calls = calls_of(&g, "check");
    assert!(calls_member(&calls, "isReady"), "calls: {calls:?}");
}

#[test]
fn test_swift_callee_continued_addition_names_method() {
    let g = parse(
        "func build() {\n    let s = (\n        head() +\n            Gen.boundary(kind: 1)\n    )\n}\n",
    );
    let calls = calls_of(&g, "build");
    assert!(calls_member(&calls, "boundary"), "calls: {calls:?}");
    assert!(calls_member(&calls, "head"), "calls: {calls:?}");
}

#[test]
fn test_swift_callee_prefix_without_call_adds_no_call() {
    let g = parse("func check() {\n    let x = !flag\n}\n");
    assert!(calls_of(&g, "check").is_empty());
}

#[test]
fn test_swift_callee_implicit_member_call_adds_no_bare_call() {
    let g = parse("func check() {\n    configure(.option(x))\n}\n");
    let calls = calls_of(&g, "check");
    assert!(!calls_member(&calls, "option"), "calls: {calls:?}");
    assert!(calls_member(&calls, "configure"), "calls: {calls:?}");
}
