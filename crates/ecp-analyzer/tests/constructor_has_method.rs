//! A constructor shares its class's name (Java, C#, C++, Dart), so the
//! file's name lookup returns the class. Class membership finds each
//! constructor by its kind and owner instead: the class has every
//! constructor, its later overloads included, and no self-loop.
//!
//! Skipped languages: the others name a constructor apart from the class
//! (`__init__`, `constructor`, `init`, `new`, `initialize`) or have none
//! (Go, Rust, C), so their lookup never returns the class.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn build(provider: &dyn LanguageProvider, path: &str, source: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(
        provider
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap(),
    );
    builder.build()
}

fn assert_class_has_both_constructors(provider: &dyn LanguageProvider, path: &str, source: &str) {
    let graph = build(provider, path, source);
    let pool = &graph.string_pool;
    let foo: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| graph.nodes[i as usize].name.resolve(pool) == "Foo")
        .collect();
    let class = *foo
        .iter()
        .find(|&&i| graph.nodes[i as usize].kind == NodeKind::Class)
        .expect("class Foo");
    let ctors: Vec<u32> = foo
        .iter()
        .copied()
        .filter(|&i| graph.nodes[i as usize].kind == NodeKind::Constructor)
        .collect();
    assert_eq!(ctors.len(), 2, "{path}: two constructor nodes");
    for ctor in ctors {
        let has = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::HasMethod && e.source == class && e.target == ctor)
            .count();
        assert_eq!(has, 1, "{path}: Foo has constructor {ctor} once");
    }
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.rel_type == RelType::HasMethod && e.source == class && e.target == class),
        "{path}: no self-loop"
    );
}

#[test]
fn test_member_node_java_constructors_share_class_name_class_has_both() {
    assert_class_has_both_constructors(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        "Foo.java",
        "class Foo {\n  Foo() {}\n  Foo(int x) {}\n}\n",
    );
}

#[test]
fn test_member_node_csharp_constructors_share_class_name_class_has_both() {
    assert_class_has_both_constructors(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        "Foo.cs",
        "class Foo {\n  public Foo() {}\n  public Foo(int x) {}\n}\n",
    );
}

#[test]
fn test_member_node_cpp_constructors_share_class_name_class_has_both() {
    assert_class_has_both_constructors(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        "foo.cpp",
        "class Foo {\npublic:\n  Foo() {}\n  Foo(int x) {}\n};\n",
    );
}

#[test]
fn test_member_node_dart_constructors_share_class_name_class_has_both() {
    assert_class_has_both_constructors(
        &ecp_analyzer::dart::parser::DartProvider::new().unwrap(),
        "foo.dart",
        "class Foo {\n  Foo() {}\n  Foo(int x) {}\n}\n",
    );
}
