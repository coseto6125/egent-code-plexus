//! Namespace, Macro, Enum, Struct, and Typedef emission for C++.
//!
//! Covers the gap categories identified in the 14-language parity audit:
//! - Macro  : `#define NAME` / `#define F(x) ...` → NodeKind::Macro
//! - Namespace: `namespace foo { }` → NodeKind::Namespace
//! - Enum   : `enum class Color {}` / `enum OldEnum {}` → NodeKind::Enum
//! - Struct : `struct Point {}` → NodeKind::Struct (not Class)
//! - Typedef: `using X = T` / `typedef T X` → NodeKind::Typedef

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CppProvider::new().expect("CppProvider init");
    let graph = provider
        .parse_file(Path::new("t.cpp"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

fn count(nodes: &[RawNode], kind: NodeKind) -> usize {
    nodes.iter().filter(|n| n.kind == kind).count()
}

// ── Macro ────────────────────────────────────────────────────────────────────

#[test]
fn macro_object_like() {
    let nodes = parse("#define MAX_SIZE 100\n");
    find(&nodes, "MAX_SIZE", NodeKind::Macro);
}

#[test]
fn macro_function_like() {
    let nodes = parse("#define SQUARE(x) ((x)*(x))\n");
    find(&nodes, "SQUARE", NodeKind::Macro);
}

#[test]
fn macro_multiple() {
    let src = "#define A 1\n#define B(x) x\n#define C\n";
    let nodes = parse(src);
    find(&nodes, "A", NodeKind::Macro);
    find(&nodes, "B", NodeKind::Macro);
    find(&nodes, "C", NodeKind::Macro);
    assert_eq!(count(&nodes, NodeKind::Macro), 3);
}

// ── Namespace ────────────────────────────────────────────────────────────────

#[test]
fn namespace_simple() {
    let nodes = parse("namespace myns { int x = 5; }\n");
    find(&nodes, "myns", NodeKind::Namespace);
}

#[test]
fn namespace_multiple() {
    let src = "namespace ns1 {}\nnamespace ns2 {}\n";
    let nodes = parse(src);
    find(&nodes, "ns1", NodeKind::Namespace);
    find(&nodes, "ns2", NodeKind::Namespace);
    assert_eq!(count(&nodes, NodeKind::Namespace), 2);
}

// ── Enum ─────────────────────────────────────────────────────────────────────

#[test]
fn enum_class() {
    let nodes = parse("enum class Color { Red, Green, Blue };\n");
    find(&nodes, "Color", NodeKind::Enum);
}

#[test]
fn enum_plain() {
    let nodes = parse("enum OldEnum { A, B, C };\n");
    find(&nodes, "OldEnum", NodeKind::Enum);
}

#[test]
fn enum_not_class() {
    let src = "enum class State { On, Off };\nenum LegacyMode { Fast, Slow };\n";
    let nodes = parse(src);
    find(&nodes, "State", NodeKind::Enum);
    find(&nodes, "LegacyMode", NodeKind::Enum);
    assert_eq!(count(&nodes, NodeKind::Enum), 2);
}

// ── Struct ───────────────────────────────────────────────────────────────────

#[test]
fn struct_emits_struct_not_class() {
    let nodes = parse("struct Point { int x; int y; };\n");
    find(&nodes, "Point", NodeKind::Struct);
    // Must NOT also appear as Class.
    assert!(
        nodes
            .iter()
            .find(|n| n.name == "Point" && n.kind == NodeKind::Class)
            .is_none(),
        "Point must not be emitted as Class"
    );
}

#[test]
fn struct_with_methods_emits_struct() {
    let nodes = parse("struct Pair { int a; int b; int sum() { return a+b; } };\n");
    find(&nodes, "Pair", NodeKind::Struct);
}

// ── Typedef / using ──────────────────────────────────────────────────────────

#[test]
fn using_alias() {
    let nodes = parse("using MyInt = int;\n");
    find(&nodes, "MyInt", NodeKind::Typedef);
}

#[test]
fn typedef_keyword() {
    let nodes = parse("typedef int MyInt;\n");
    find(&nodes, "MyInt", NodeKind::Typedef);
}
