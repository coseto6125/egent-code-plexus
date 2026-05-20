//! Regression coverage for queries.scm methods / functions whose return type
//! is wrapped in a pointer or reference declarator.
//!
//! Tree-sitter-cpp parses `T* foo()` as:
//!   function_definition
//!     type: T
//!     declarator: pointer_declarator           ← outer wrapper
//!       declarator: function_declarator
//!         declarator: field_identifier "foo"
//!
//! Before the fix the Method / Function patterns only matched when the
//! outermost declarator was `function_declarator`, so any method or free
//! function returning `T*` / `T&` was silently dropped — `type_name`,
//! `parse_error`, `json_pointer` and many other nlohmann::json members
//! disappeared from the graph.

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

// ── Methods inside class body ────────────────────────────────────────────────

#[test]
fn method_returning_plain_type_emitted() {
    let nodes = parse("class Foo { int simple() { return 0; } };\n");
    find(&nodes, "simple", NodeKind::Method);
}

#[test]
fn method_returning_void_emitted() {
    let nodes = parse("class Foo { void run() {} };\n");
    find(&nodes, "run", NodeKind::Method);
}

#[test]
fn method_returning_pointer_emitted() {
    let nodes = parse("class Foo { const char* type_name() const noexcept { return \"x\"; } };\n");
    find(&nodes, "type_name", NodeKind::Method);
}

#[test]
fn method_returning_reference_emitted() {
    let nodes = parse("class Foo { Foo& assign(int x) { return *this; } };\n");
    find(&nodes, "assign", NodeKind::Method);
}

#[test]
fn method_returning_rvalue_reference_emitted() {
    let nodes = parse("class Foo { Foo&& move_out() { return Foo(); } };\n");
    find(&nodes, "move_out", NodeKind::Method);
}

#[test]
fn method_returning_double_pointer_emitted() {
    let nodes = parse("class Foo { int** matrix() { return nullptr; } };\n");
    find(&nodes, "matrix", NodeKind::Method);
}

// ── Class-body prototypes (declaration without body) ─────────────────────────

#[test]
fn method_prototype_returning_pointer_emitted() {
    let nodes = parse("class Foo { const char* type_name() const; };\n");
    find(&nodes, "type_name", NodeKind::Method);
}

#[test]
fn method_prototype_returning_reference_emitted() {
    let nodes = parse("class Foo { Foo& assign(int x); };\n");
    find(&nodes, "assign", NodeKind::Method);
}

// ── Out-of-class definitions (qualified_identifier) ──────────────────────────

#[test]
fn out_of_class_method_returning_pointer_emitted() {
    let nodes = parse(
        "class Foo { const char* type_name() const; };\n\
         const char* Foo::type_name() const { return \"x\"; }\n",
    );
    let n = nodes
        .iter()
        .filter(|n| n.name == "type_name" && n.kind == NodeKind::Method)
        .count();
    assert!(
        n >= 2,
        "expected ≥2 type_name Method nodes (decl + defn), got {n}: {nodes:#?}"
    );
}

#[test]
fn out_of_class_method_returning_reference_emitted() {
    let nodes = parse(
        "class Foo { Foo& assign(int x); };\n\
         Foo& Foo::assign(int x) { return *this; }\n",
    );
    let n = nodes
        .iter()
        .filter(|n| n.name == "assign" && n.kind == NodeKind::Method)
        .count();
    assert!(
        n >= 2,
        "expected ≥2 assign Method nodes (decl + defn), got {n}: {nodes:#?}"
    );
}

// ── Free functions at translation-unit scope ─────────────────────────────────

#[test]
fn free_function_returning_pointer_emitted() {
    let nodes = parse("int* alloc_int() { return nullptr; }\n");
    find(&nodes, "alloc_int", NodeKind::Function);
}

#[test]
fn free_function_returning_reference_emitted() {
    let nodes = parse("int& get_ref() { static int x; return x; }\n");
    find(&nodes, "get_ref", NodeKind::Function);
}

#[test]
fn free_function_prototype_returning_pointer_emitted() {
    let nodes = parse("int* alloc_int();\n");
    find(&nodes, "alloc_int", NodeKind::Function);
}

#[test]
fn free_function_prototype_returning_reference_emitted() {
    let nodes = parse("int& get_ref();\n");
    find(&nodes, "get_ref", NodeKind::Function);
}
