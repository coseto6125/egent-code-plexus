//! Functions that share a source line (minified JS, `void a(){x();} void b(){y();}`)
//! each keep their own calls: attribution compares `(row, column)`, not the row.
//! Every fixture puts `a` (calling `x`) and `b` (calling `y`) on one line.

use ecp_analyzer::calls::VariantRef;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(provider: &dyn LanguageProvider, path: &str, source: &str) -> LocalGraph {
    provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap()
}

fn callable<'g>(graph: &'g LocalGraph, name: &str) -> &'g RawNode {
    graph
        .nodes
        .iter()
        .find(|n| {
            n.name == name
                && matches!(
                    n.kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                )
        })
        .unwrap_or_else(|| panic!("no callable `{name}`"))
}

fn holds(node: &RawNode, callee: &str) -> bool {
    node.calls.iter().any(|c| c == callee)
}

fn assert_each_keeps_own_call(provider: &dyn LanguageProvider, path: &str, source: &str) {
    let graph = parse(provider, path, source);
    let (a, b) = (callable(&graph, "a"), callable(&graph, "b"));
    assert!(holds(a, "x"), "{path}: `a` holds `x`, got {:?}", a.calls);
    assert!(
        !holds(a, "y"),
        "{path}: `a` must not hold `y`: {:?}",
        a.calls
    );
    assert!(holds(b, "y"), "{path}: `b` holds `y`, got {:?}", b.calls);
    assert!(
        !holds(b, "x"),
        "{path}: `b` must not hold `x`: {:?}",
        b.calls
    );
}

#[test]
fn test_attach_calls_typescript_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        "m.ts",
        "function a(){ x(); } function b(){ y(); }",
    );
}

#[test]
fn test_attach_calls_javascript_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        "m.js",
        "function a(){ x(); } function b(){ y(); }",
    );
}

#[test]
fn test_attach_calls_javascript_nested_same_line_function_splits_parent_and_child() {
    let graph = parse(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        "m.js",
        "function outer(){ const f = function inner(){ y() }; x() }",
    );
    let outer = callable(&graph, "outer");
    assert!(holds(outer, "x"), "outer holds `x`: {:?}", outer.calls);
    assert!(
        !holds(outer, "y"),
        "outer must not hold `y`: {:?}",
        outer.calls
    );
    let owner_of_y = graph
        .nodes
        .iter()
        .find(|n| holds(n, "y"))
        .expect("some function holds `y`");
    assert_ne!(owner_of_y.name, "outer");
    assert!(!holds(owner_of_y, "x"));
}

// Python cannot place two `def` headers on one line (a compound statement
// needs its own line), so the shared-line case is infeasible; this pins the
// one-line-body case on adjacent lines instead.
#[test]
fn test_attach_calls_python_one_line_bodies_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::python::PythonProvider::new().unwrap(),
        "m.py",
        "def a(): x()\ndef b(): y()\n",
    );
}

#[test]
fn test_attach_calls_java_same_line_methods_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        "K.java",
        "class K { void a(){ x(); } void b(){ y(); } }",
    );
}

#[test]
fn test_attach_calls_kotlin_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        "m.kt",
        "fun a() { x() }; fun b() { y() }",
    );
}

#[test]
fn test_attach_calls_csharp_same_line_methods_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        "K.cs",
        "class K { void a(){ x(); } void b(){ y(); } }",
    );
}

#[test]
fn test_attach_calls_go_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::go::parser::GoProvider::new().unwrap(),
        "m.go",
        "package p\nfunc a() { x() }; func b() { y() }\n",
    );
}

#[test]
fn test_attach_calls_rust_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        "m.rs",
        "fn a(){ x(); } fn b(){ y(); }",
    );
}

#[test]
fn test_attach_calls_php_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
        "m.php",
        "<?php function a(){ x(); } function b(){ y(); }",
    );
}

#[test]
fn test_attach_calls_ruby_same_line_methods_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::ruby::parser::RubyProvider::new().unwrap(),
        "m.rb",
        "def a; x(); end; def b; y(); end\n",
    );
}

#[test]
fn test_attach_calls_swift_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::swift::parser::SwiftProvider::new().unwrap(),
        "m.swift",
        "func a() { x() }; func b() { y() }\n",
    );
}

#[test]
fn test_attach_calls_c_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::c::parser::CProvider::new().unwrap(),
        "m.c",
        "void a(){ x(); } void b(){ y(); }\n",
    );
}

#[test]
fn test_attach_calls_cpp_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        "m.cpp",
        "void a(){ x(); } void b(){ y(); }\n",
    );
}

#[test]
fn test_attach_calls_dart_same_line_functions_keep_own_calls() {
    assert_each_keeps_own_call(
        &ecp_analyzer::dart::parser::DartProvider::new().unwrap(),
        "m.dart",
        "void a() { x(); } void b() { y(); }\n",
    );
}

#[test]
fn test_extract_variant_refs_rust_same_line_functions_keep_own_variants() {
    let graph = parse(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        "m.rs",
        "enum E { A, B }\nfn a(){ let _ = E::A; } fn b(){ let _ = E::B; }\n",
    );
    let paths = |name: &str| -> Vec<String> {
        callable(&graph, name)
            .field_reads
            .iter()
            .filter_map(|r| VariantRef::parse(r).map(|v| v.path.to_string()))
            .collect()
    };
    assert_eq!(paths("a"), ["E::A"]);
    assert_eq!(paths("b"), ["E::B"]);
}
