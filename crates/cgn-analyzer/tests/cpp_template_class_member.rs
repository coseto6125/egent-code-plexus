//! `template <typename T> class C { void f() {...} };` — class member functions
//! defined inside a class wrapped by `template_declaration` must still emit
//! as Method. Previously cgn's cpp/queries.scm only matched
//! `function_definition` nodes whose direct parent was the class body, missing
//! the case where the whole class sits under `template_declaration`.
//!
//! Repro: nlohmann/json.hpp `template <...> class basic_json { reference at(...) }`
//! — `at` shows up in ref-gitnexus as Template but cgn emits nothing
//! for `(json.hpp, at)`.

use cgn_analyzer::cpp::parser::CppProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;
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
fn template_class_inline_member_is_method() {
    let nodes = parse("template <typename T> class C { void f() {} };\n");
    find(&nodes, "f", NodeKind::Method);
}

#[test]
fn template_class_inline_member_with_return_type_is_method() {
    let nodes = parse(
        "template <typename T> class C { int compute(int x) { return x; } };\n",
    );
    find(&nodes, "compute", NodeKind::Method);
}

#[test]
fn template_struct_inline_member_is_method() {
    let nodes = parse("template <typename T> struct S { void g() {} };\n");
    find(&nodes, "g", NodeKind::Method);
}

#[test]
fn template_function_at_namespace_scope_is_function() {
    let nodes = parse("template <typename T> T add(T a, T b) { return a + b; }\n");
    find(&nodes, "add", NodeKind::Function);
}

#[test]
fn template_class_multiple_members_all_emit() {
    let nodes = parse(
        "template <typename T> class C {\n\
             public:\n\
             T at(int i) { return T{}; }\n\
             bool empty() const { return true; }\n\
             void clear() {}\n\
         };\n",
    );
    find(&nodes, "at", NodeKind::Method);
    find(&nodes, "empty", NodeKind::Method);
    find(&nodes, "clear", NodeKind::Method);
}
