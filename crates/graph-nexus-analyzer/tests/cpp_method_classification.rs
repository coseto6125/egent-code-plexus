//! Method classification for inline class/struct member functions.
//!
//! The gap: `function_definition` nodes whose lexical parent is a
//! `field_declaration_list` (i.e., inside a class or struct body) must emit
//! `NodeKind::Method`, not `NodeKind::Function`.  This covers:
//! - Regular member functions defined inline in the class body.
//! - Constructors and destructors (tree-sitter aliases them to
//!   `function_definition`).
//! - Operator overloads defined inline.
//!
//! Free functions at file scope must remain `NodeKind::Function`.

use graph_nexus_analyzer::cpp::parser::CppProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
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

fn absent(nodes: &[RawNode], name: &str, kind: NodeKind) {
    assert!(
        nodes
            .iter()
            .find(|n| n.name == name && n.kind == kind)
            .is_none(),
        "unexpected {kind:?} `{name}` found in {nodes:#?}"
    );
}

// ── Inline member functions → Method ────────────────────────────────────────

#[test]
fn inline_member_function_is_method() {
    let nodes = parse("class Foo { void bar() {} };\n");
    find(&nodes, "bar", NodeKind::Method);
    absent(&nodes, "bar", NodeKind::Function);
}

#[test]
fn inline_struct_member_function_is_method() {
    let nodes = parse("struct S { int compute() { return 0; } };\n");
    find(&nodes, "compute", NodeKind::Method);
    absent(&nodes, "compute", NodeKind::Function);
}

#[test]
fn inline_constructor_is_method() {
    // Constructor defined inline: tree-sitter aliases it as function_definition
    // inside the class body → must become Method.
    let nodes = parse("struct person { person(int x) {} };\n");
    find(&nodes, "person", NodeKind::Method);
}

#[test]
fn multiple_inline_methods() {
    let src = "class C { void a() {} int b() { return 1; } };\n";
    let nodes = parse(src);
    find(&nodes, "a", NodeKind::Method);
    find(&nodes, "b", NodeKind::Method);
    absent(&nodes, "a", NodeKind::Function);
    absent(&nodes, "b", NodeKind::Function);
}

// ── Free functions stay Function ─────────────────────────────────────────────

#[test]
fn free_function_stays_function() {
    let nodes = parse("void helper() {}\n");
    find(&nodes, "helper", NodeKind::Function);
    absent(&nodes, "helper", NodeKind::Method);
}

#[test]
fn out_of_line_method_stays_method() {
    // Qualified out-of-line definition: `void Foo::bar() {}` — already
    // matched by the qualified-identifier @name.method pattern.
    let nodes = parse("class Foo {};\nvoid Foo::bar() {}\n");
    find(&nodes, "bar", NodeKind::Method);
    absent(&nodes, "bar", NodeKind::Function);
}

// ── Nested class still classifies correctly ──────────────────────────────────

#[test]
fn nested_class_inline_method_is_method() {
    let src = "class Outer { class Inner { void fn() {} }; };\n";
    let nodes = parse(src);
    find(&nodes, "fn", NodeKind::Method);
    absent(&nodes, "fn", NodeKind::Function);
}

// ── Class declared inside namespace ──────────────────────────────────────────

#[test]
fn method_inside_namespace_class() {
    let src = "namespace ns { struct person { person(int x) {} }; }\n";
    let nodes = parse(src);
    find(&nodes, "person", NodeKind::Method);
    absent(&nodes, "person", NodeKind::Function);
}
