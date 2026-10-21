//! A call through a receiver of a declared type (`self.m()` typed `C.m`,
//! Java `this.m()`, Go `c.M()` on `c *C`) reaches only that type's members
//! and its supertypes' (`PickScope::qualifier_is_type`). When neither the
//! type nor a supertype declares the member, the same-file pick binds no
//! free function and no member of an unrelated type of the file, and the
//! receiver-typing ladder decides.
//!
//! - no lineage member: `Other` declares `m`, `C` calls `this.m()` → no edge.
//! - inherited member: `C` extends `Mid` extends `Base`, `Other` and `Base`
//!   declare `m`, `C` calls `this.m()` → `Base.m`, the nearest supertype's.
//! - members on two branches: Python `D(B, C)`, `B(A)`, `A` and `C` declare
//!   `m` → never `C.m`, which breadth-first order meets first but the MRO
//!   (D, B, A, C) does not reach.
//!
//! Skipped languages: C has no members. Kotlin extension functions, C#
//! extension methods, Dart `extension on` members and Rust blanket impls
//! reach past a type's lineage, so those languages keep the by-name rules;
//! `receiver_emission.rs`
//! (`test_resolve_call_kotlin_extension_on_type_receiver_binds_top_level_function`)
//! and `same_file_ambiguous_owner.rs` guard them.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
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

/// Owner classes of the `callee` nodes that the one node named `caller`
/// calls, sorted.
fn owners_called(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<String> {
    let pool = &graph.string_pool;
    let name = |i: u32| graph.nodes[i as usize].name.resolve(pool);
    let callers: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| name(i) == caller)
        .collect();
    assert_eq!(callers.len(), 1, "one `{caller}` node");
    let mut owners: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == callers[0])
        .filter(|e| name(e.target) == callee)
        .map(|e| {
            graph.nodes[e.target as usize]
                .owner_class
                .resolve(pool)
                .to_string()
        })
        .collect();
    owners.sort();
    owners
}

fn assert_no_edge(
    provider: &dyn LanguageProvider,
    path: &str,
    source: &str,
    caller: &str,
    callee: &str,
) {
    let graph = build(provider, path, source);
    assert_eq!(
        owners_called(&graph, caller, callee),
        Vec::<String>::new(),
        "{caller} binds no `{callee}` outside its type's lineage"
    );
}

fn typescript() -> ecp_analyzer::typescript::TypeScriptProvider {
    ecp_analyzer::typescript::TypeScriptProvider::new().unwrap()
}
fn javascript() -> ecp_analyzer::javascript::parser::JavaScriptProvider {
    ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap()
}
fn python() -> ecp_analyzer::python::PythonProvider {
    ecp_analyzer::python::PythonProvider::new().unwrap()
}
fn java() -> ecp_analyzer::java::parser::JavaProvider {
    ecp_analyzer::java::parser::JavaProvider::new().unwrap()
}
fn go() -> ecp_analyzer::go::parser::GoProvider {
    ecp_analyzer::go::parser::GoProvider::new().unwrap()
}
fn php() -> ecp_analyzer::php::parser::PhpProvider {
    ecp_analyzer::php::parser::PhpProvider::new().unwrap()
}
fn ruby() -> ecp_analyzer::ruby::parser::RubyProvider {
    ecp_analyzer::ruby::parser::RubyProvider::new().unwrap()
}
fn swift() -> ecp_analyzer::swift::parser::SwiftProvider {
    ecp_analyzer::swift::parser::SwiftProvider::new().unwrap()
}
fn cpp() -> ecp_analyzer::cpp::parser::CppProvider {
    ecp_analyzer::cpp::parser::CppProvider::new().unwrap()
}

// ─── No lineage member ──────────────────────────────────────────────────────

/// The review's repro: `Article.publish` called `Comment.save` (0.85).
#[test]
fn test_resolve_call_python_self_member_of_unrelated_class_binds_nothing() {
    assert_no_edge(
        &python(),
        "blog/models.py",
        "from django.db import models\nclass Comment(models.Model):\n    def save(self, *a, **k): ...\nclass Article(models.Model):\n    def publish(self): self.save()\n",
        "publish",
        "save",
    );
}

#[test]
fn test_resolve_call_python_self_member_named_like_free_function_binds_nothing() {
    assert_no_edge(
        &python(),
        "u.py",
        "def helper():\n    pass\nclass A:\n    def run(self):\n        self.helper()\n",
        "run",
        "helper",
    );
}

#[test]
fn test_resolve_call_java_this_member_of_unrelated_class_binds_nothing() {
    assert_no_edge(
        &java(),
        "U.java",
        "class Other {\n  void m() {}\n}\nclass C {\n  void run() {\n    this.m();\n  }\n}\n",
        "run",
        "m",
    );
}

#[test]
fn test_resolve_call_typescript_this_member_of_unrelated_class_binds_nothing() {
    assert_no_edge(
        &typescript(),
        "u.ts",
        "class Other { m() {} }\nclass C { run() { this.m(); } }\n",
        "run",
        "m",
    );
}

#[test]
fn test_resolve_call_javascript_this_member_of_unrelated_class_binds_nothing() {
    assert_no_edge(
        &javascript(),
        "u.js",
        "class Other { m() {} }\nclass C { run() { this.m(); } }\n",
        "run",
        "m",
    );
}

#[test]
fn test_resolve_call_php_this_member_of_unrelated_class_binds_nothing() {
    assert_no_edge(
        &php(),
        "u.php",
        "<?php\nclass Other { function m() {} }\nclass C { function run() { $this->m(); } }\n",
        "run",
        "m",
    );
}

#[test]
fn test_resolve_call_ruby_self_member_of_unrelated_class_binds_nothing() {
    assert_no_edge(
        &ruby(),
        "u.rb",
        "class Other\n  def m\n  end\nend\nclass C\n  def run\n    self.m\n  end\nend\n",
        "run",
        "m",
    );
}

#[test]
fn test_resolve_call_go_receiver_member_of_unrelated_type_binds_nothing() {
    assert_no_edge(
        &go(),
        "u.go",
        "package p\n\ntype Other struct{}\n\nfunc (o *Other) M() {}\n\ntype C struct{}\n\nfunc (c *C) Run() {\n\tc.M()\n}\n",
        "Run",
        "M",
    );
}

#[test]
fn test_resolve_call_swift_self_member_of_unrelated_class_binds_nothing() {
    assert_no_edge(
        &swift(),
        "U.swift",
        "class Other { func m() {} }\nclass C { func run() { self.m() } }\n",
        "run",
        "m",
    );
}

#[test]
fn test_resolve_call_cpp_this_member_of_unrelated_class_binds_nothing() {
    assert_no_edge(
        &cpp(),
        "u.cpp",
        "class Other { public: void m() {} };\nclass C { public: void run() { this->m(); } };\n",
        "run",
        "m",
    );
}

// ─── Inherited member ───────────────────────────────────────────────────────

#[test]
fn test_resolve_call_python_self_member_of_grandparent_binds_grandparent() {
    let graph = build(
        &python(),
        "u.py",
        "class Other:\n    def save(self):\n        pass\nclass Base:\n    def save(self):\n        pass\nclass Mid(Base):\n    pass\nclass Leaf(Mid):\n    def run(self):\n        self.save()\n",
    );
    assert_eq!(owners_called(&graph, "run", "save"), ["Base"]);
}

#[test]
fn test_resolve_call_python_self_member_on_two_base_branches_binds_no_second_branch() {
    let graph = build(
        &python(),
        "u.py",
        "class A:\n    def m(self):\n        pass\nclass B(A):\n    pass\nclass C:\n    def m(self):\n        pass\nclass D(B, C):\n    def run(self):\n        self.m()\n",
    );
    let owners = owners_called(&graph, "run", "m");
    assert!(!owners.contains(&"C".to_string()), "{owners:?}");
}

#[test]
fn test_resolve_call_java_this_member_of_grandparent_binds_grandparent() {
    let graph = build(
        &java(),
        "U.java",
        "class Other {\n  void m() {}\n}\nclass Base {\n  void m() {}\n}\nclass Mid extends Base {}\nclass C extends Mid {\n  void run() {\n    this.m();\n  }\n}\n",
    );
    assert_eq!(owners_called(&graph, "run", "m"), ["Base"]);
}
