//! Functions that share a source line (minified JS, `void a(){x();} void b(){y();}`)
//! each keep their own calls: attribution compares `(row, column)`, not the row.
//! Every fixture puts `a` (calling `x`) and `b` (calling `y`) on one line.

use ecp_analyzer::calls::VariantRef;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
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

// Receiver typing scopes locals by position: two functions on one line each
// declare a local `k` of a different type, and `k.save()` binds the type of
// the `k` in the caller's own function.

fn build(provider: &dyn LanguageProvider, path: &str, source: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse(provider, path, source));
    builder.build()
}

fn named(graph: &ZeroCopyGraph, name: &str) -> Vec<u32> {
    (0..graph.nodes.len() as u32)
        .filter(|&i| graph.nodes[i as usize].name.resolve(&graph.string_pool) == name)
        .collect()
}

/// Owner classes of the `save` nodes `caller` calls.
fn save_owners_called_by(graph: &ZeroCopyGraph, caller: u32) -> Vec<String> {
    let pool = &graph.string_pool;
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == caller)
        .map(|e| &graph.nodes[e.target as usize])
        .filter(|t| t.name.resolve(pool) == "save")
        .map(|t| t.owner_class.resolve(pool).to_string())
        .collect()
}

fn assert_local_scopes_split_by_column(provider: &dyn LanguageProvider, path: &str, source: &str) {
    let graph = build(provider, path, source);
    for (func, owner) in [("a", "A"), ("b", "B")] {
        let caller = named(&graph, func)[0];
        let owners = save_owners_called_by(&graph, caller);
        assert!(
            !owners.is_empty() && owners.iter().all(|o| o == owner),
            "{path}: `{func}` binds {owner}'s `save`, got {owners:?}"
        );
    }
}

/// Two classes on one line, each with a `run` calling its own `save` through
/// `self` / `$this`.
fn assert_class_scopes_split_by_column(provider: &dyn LanguageProvider, path: &str, source: &str) {
    let graph = build(provider, path, source);
    let pool = &graph.string_pool;
    for owner in ["A", "B"] {
        let run = named(&graph, "run")
            .into_iter()
            .find(|&i| graph.nodes[i as usize].owner_class.resolve(pool) == owner)
            .unwrap_or_else(|| panic!("{path}: no `run` owned by {owner}"));
        let owners = save_owners_called_by(&graph, run);
        assert!(
            !owners.is_empty() && owners.iter().all(|o| o == owner),
            "{path}: {owner}::run binds {owner}'s `save`, got {owners:?}"
        );
    }
}

// JavaScript has no type annotations and no local-type scopes, Python cannot
// place two `def` headers on one line, and C has no methods: no case for them.

#[test]
fn test_resolve_receiver_typescript_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        "m.ts",
        "class A { save(){} }\nclass B { save(){} }\nfunction a(){ const k: A = new A(); k.save(); } function b(){ const k: B = new B(); k.save(); }\n",
    );
}

#[test]
fn test_resolve_receiver_java_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        "K.java",
        "class A { void save(){} }\nclass B { void save(){} }\nclass K { void a(){ A k = new A(); k.save(); } void b(){ B k = new B(); k.save(); } }\n",
    );
}

#[test]
fn test_resolve_receiver_kotlin_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        "m.kt",
        "class A { fun save() {} }\nclass B { fun save() {} }\nfun a() { val k: A = A(); k.save() }; fun b() { val k: B = B(); k.save() }\n",
    );
}

#[test]
fn test_resolve_receiver_csharp_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        "K.cs",
        "class A { public void save(){} }\nclass B { public void save(){} }\nclass K { void a(){ A k = new A(); k.save(); } void b(){ B k = new B(); k.save(); } }\n",
    );
}

#[test]
fn test_resolve_receiver_go_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::go::parser::GoProvider::new().unwrap(),
        "m.go",
        "package p\ntype A struct{}\nfunc (A) save() {}\ntype B struct{}\nfunc (B) save() {}\nfunc a() { var k A; k.save() }; func b() { var k B; k.save() }\n",
    );
}

#[test]
fn test_resolve_receiver_rust_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        "m.rs",
        "struct A;\nimpl A { fn save(&self) {} }\nstruct B;\nimpl B { fn save(&self) {} }\nfn a(){ let k: A = A; k.save(); } fn b(){ let k: B = B; k.save(); }\n",
    );
}

#[test]
fn test_resolve_receiver_swift_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::swift::parser::SwiftProvider::new().unwrap(),
        "m.swift",
        "class A { func save() {} }\nclass B { func save() {} }\nfunc a() { let k: A = A(); k.save() }; func b() { let k: B = B(); k.save() }\n",
    );
}

#[test]
fn test_resolve_receiver_cpp_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        "m.cpp",
        "struct A { void save() {} };\nstruct B { void save() {} };\nvoid a(){ A k; k.save(); } void b(){ B k; k.save(); }\n",
    );
}

#[test]
fn test_resolve_receiver_dart_same_line_locals_bind_own_type() {
    assert_local_scopes_split_by_column(
        &ecp_analyzer::dart::parser::DartProvider::new().unwrap(),
        "m.dart",
        "class A { void save() {} }\nclass B { void save() {} }\nvoid a() { A k = A(); k.save(); } void b() { B k = B(); k.save(); }\n",
    );
}

// PHP and Ruby type no locals; their receiver typing reads the enclosing
// class, so the same-line case is two classes on one line.

#[test]
fn test_resolve_receiver_php_same_line_classes_bind_own_this() {
    assert_class_scopes_split_by_column(
        &ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
        "m.php",
        "<?php class A { function save(){} function run(){ $this->save(); } } class B { function save(){} function run(){ $this->save(); } }\n",
    );
}

#[test]
fn test_resolve_receiver_ruby_same_line_classes_bind_own_self() {
    assert_class_scopes_split_by_column(
        &ecp_analyzer::ruby::parser::RubyProvider::new().unwrap(),
        "m.rb",
        "class A; def save; end; def run; self.save; end; end; class B; def save; end; def run; self.save; end; end\n",
    );
}
