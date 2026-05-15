//! Type annotations on C++ nodes (params, fields, return types, vars, methods).
//!
//! Covers Wave 2 task D3 from
//! `docs/specs/2026-05-15-language-coverage-gaps.md`.
//!
//! Conventions documented in `src/cpp/parser.rs::slice_type_before`:
//! - **Pointer / reference spacing**: preserved as-written. `char* s` →
//!   `"char*"`, `const std::string& s` → `"const std::string&"`.
//! - **Qualifier inclusion**: full prefix kept (storage-class, cv).
//! - **Templates**: preserved verbatim (`std::vector<int>`,
//!   `std::map<std::string,User>`).
//! - **`auto`**: kept literally — the analyzer does not perform deduction.

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

#[test]
fn param_template() {
    let nodes = parse("void f(std::vector<int> v);\n");
    let p = find(&nodes, "v", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("std::vector<int>"));
}

#[test]
fn param_reference() {
    // Source spelling `const std::string& s` is preserved — the `&` lives
    // in the declarator wrapper but the source-slice approach captures
    // everything before the identifier name.
    let nodes = parse("void f(const std::string& s);\n");
    let p = find(&nodes, "s", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("const std::string&"));
}

#[test]
fn return_type_template() {
    let nodes = parse("std::map<std::string,User> getAll();\n");
    let f = find(&nodes, "getAll", NodeKind::Function);
    assert_eq!(
        f.type_annotation.as_deref(),
        Some("std::map<std::string,User>")
    );
}

#[test]
fn class_field() {
    let nodes = parse("class C { int x; };\n");
    let x = find(&nodes, "x", NodeKind::Property);
    assert_eq!(x.type_annotation.as_deref(), Some("int"));
}

#[test]
fn auto_var() {
    // Documented choice: keep `auto` literal. Deducing the actual type from
    // the initializer requires semantic analysis the analyzer doesn't perform.
    let nodes = parse("auto x = 5;\n");
    let v = find(&nodes, "x", NodeKind::Variable);
    assert_eq!(v.type_annotation.as_deref(), Some("auto"));
}

#[test]
fn member_function_return() {
    // Member function declared inside a class body — `class C { int sum(); };`
    // — must emit a Method node carrying the return-type annotation.
    let nodes = parse("class C { int sum(); };\n");
    let m = find(&nodes, "sum", NodeKind::Method);
    assert_eq!(m.type_annotation.as_deref(), Some("int"));
}

#[test]
fn param_primitive() {
    let nodes = parse("void f(int x);\n");
    let p = find(&nodes, "x", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("int"));
}

#[test]
fn class_field_template() {
    let nodes = parse("class C { std::vector<int> items; };\n");
    let p = find(&nodes, "items", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("std::vector<int>"));
}
