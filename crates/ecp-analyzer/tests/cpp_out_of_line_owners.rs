//! C++ out-of-line member definitions whose class a header declares
//! (`void A::reset() {}` in `a.cpp`). The qualifier owns the definition, so
//! two classes' members of one name are two declarations with distinct
//! uids, never one overload set (`References … overload_set`). Inside a
//! `namespace { }` block the qualifier still owns it, not the namespace.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn build(path: &str, source: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(
        CppProvider::new()
            .unwrap()
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap(),
    );
    builder.build()
}

/// `(owner_class, node index)` of each Method named `name`, by owner.
fn methods(graph: &ZeroCopyGraph, name: &str) -> Vec<(String, u32)> {
    let pool = &graph.string_pool;
    let mut found: Vec<(String, u32)> = (0..graph.nodes.len() as u32)
        .filter(|&i| {
            let node = &graph.nodes[i as usize];
            node.name.resolve(pool) == name && node.kind == NodeKind::Method
        })
        .map(|i| {
            let owner = graph.nodes[i as usize]
                .owner_class
                .resolve(pool)
                .to_string();
            (owner, i)
        })
        .collect();
    found.sort();
    found
}

fn overload_edges(graph: &ZeroCopyGraph) -> usize {
    let pool = &graph.string_pool;
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::References && e.reason.resolve(pool) == "overload_set")
        .count()
}

fn assert_two_owners(source: &str, name: &str) {
    let graph = build("src/a.cpp", source);
    let found = methods(&graph, name);
    let owners: Vec<&str> = found.iter().map(|(owner, _)| owner.as_str()).collect();
    assert_eq!(owners, ["A", "B"], "{name}: one Method per class");
    let uid = |i: u32| graph.nodes[i as usize].uid;
    assert_ne!(uid(found[0].1), uid(found[1].1), "{name}: distinct uids");
    assert_eq!(overload_edges(&graph), 0, "{name}: no overload set");
}

#[test]
fn test_parse_file_cpp_header_classes_out_of_line_same_name_are_two_methods() {
    assert_two_owners("void A::reset() {}\nvoid B::reset() {}\n", "reset");
}

#[test]
fn test_parse_file_cpp_header_classes_out_of_line_in_namespace_are_owned_by_class() {
    assert_two_owners(
        "namespace ns {\nvoid A::reset() {}\nvoid B::reset() {}\n}\n",
        "reset",
    );
}

#[test]
fn test_parse_file_cpp_header_classes_out_of_line_operators_are_two_methods() {
    assert_two_owners(
        "bool A::operator==(const A& o) const { return true; }\nbool B::operator==(const B& o) const { return true; }\n",
        "operator==",
    );
}

/// One header class's two out-of-line overloads stay one overload set.
#[test]
fn test_parse_file_cpp_header_class_out_of_line_overloads_keep_overload_set() {
    let graph = build(
        "src/a.cpp",
        "void A::save(int id) {}\nvoid A::save(const char* name) {}\n",
    );
    let found = methods(&graph, "save");
    let owners: Vec<&str> = found.iter().map(|(owner, _)| owner.as_str()).collect();
    assert_eq!(owners, ["A", "A"]);
    assert_eq!(
        overload_edges(&graph),
        1,
        "the first `save` references the later one"
    );
}
