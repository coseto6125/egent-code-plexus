//! A call through a type (`self.save()`, `this.save()`, `$this->save()`,
//! an implicit-`this` `save()`) binds that type's own member, never a
//! same-named interface, trait, protocol or base declaration that precedes
//! it in the file. Each fixture declares `A` with `save` first, then `K`
//! with its own `save` (which calls `audit`) and `run`, which calls `save`.
//!
//! C has no methods, so it has no fixture.

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

fn named(graph: &ZeroCopyGraph, name: &str) -> Vec<u32> {
    (0..graph.nodes.len() as u32)
        .filter(|&i| graph.nodes[i as usize].name.resolve(&graph.string_pool) == name)
        .collect()
}

/// The `save` nodes `run` calls.
fn saves_called_by_run(graph: &ZeroCopyGraph) -> Vec<u32> {
    let pool = &graph.string_pool;
    let run = named(graph, "run")[0];
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == run)
        .map(|e| e.target)
        .filter(|&t| graph.nodes[t as usize].name.resolve(pool) == "save")
        .collect()
}

fn assert_run_binds_own_save(provider: &dyn LanguageProvider, path: &str, source: &str) {
    let graph = build(provider, path, source);
    let pool = &graph.string_pool;
    let targets = saves_called_by_run(&graph);
    assert!(!targets.is_empty(), "{path}: `run` calls `save`");
    for target in targets {
        assert_eq!(
            graph.nodes[target as usize].owner_class.resolve(pool),
            "K",
            "{path}: `run` binds K's own `save`, not A's declaration"
        );
    }
}

/// Rust: an inherent method takes precedence over a trait method, so
/// `self.save()` binds the `impl K` one, not the trait declaration and not
/// the `impl A for K` one (which alone calls `audit`).
#[test]
fn test_resolve_call_rust_trait_declared_first_binds_inherent_method() {
    let graph = build(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        "k.rs",
        "struct K;\ntrait A { fn save(&self); }\nimpl K { fn audit(&self) {} fn save(&self) {} fn run(&self) { self.save(); } }\nimpl A for K { fn save(&self) { self.audit(); } }\n",
    );
    let pool = &graph.string_pool;
    let audit = named(&graph, "audit")[0];
    let targets = saves_called_by_run(&graph);
    assert_eq!(targets.len(), 1, "one Calls edge for `self.save()`");
    let save = targets[0];
    assert_eq!(graph.nodes[save as usize].owner_class.resolve(pool), "K");
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.rel_type == RelType::Calls && e.source == save && e.target == audit),
        "the bound `save` is the inherent one, which calls nothing"
    );
}

#[test]
fn test_resolve_call_java_interface_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        "K.java",
        "interface A { void save(); }\nclass K implements A {\n  void audit() {}\n  public void save() { audit(); }\n  void run() { this.save(); save(); }\n}\n",
    );
}

#[test]
fn test_resolve_call_kotlin_interface_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        "K.kt",
        "interface A { fun save() }\nclass K : A {\n  fun audit() {}\n  override fun save() { audit() }\n  fun run() { this.save(); save() }\n}\n",
    );
}

#[test]
fn test_resolve_call_csharp_interface_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        "K.cs",
        "interface A { void save(); }\nclass K : A {\n  void audit() {}\n  public void save() { audit(); }\n  void run() { this.save(); save(); }\n}\n",
    );
}

#[test]
fn test_resolve_call_typescript_interface_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        "k.ts",
        "interface A { save(): void; }\nclass K implements A {\n  audit() {}\n  save() { this.audit(); }\n  run() { this.save(); }\n}\n",
    );
}

/// JavaScript has no interfaces: a base class declared first stands in.
#[test]
fn test_resolve_call_javascript_base_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        "k.js",
        "class A { save() {} }\nclass K extends A {\n  audit() {}\n  save() { this.audit(); }\n  run() { this.save(); }\n}\n",
    );
}

/// Python has no interfaces: a base class declared first stands in.
#[test]
fn test_resolve_call_python_base_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::python::PythonProvider::new().unwrap(),
        "k.py",
        "class A:\n    def save(self):\n        pass\nclass K(A):\n    def audit(self):\n        pass\n    def save(self):\n        self.audit()\n    def run(self):\n        self.save()\n",
    );
}

#[test]
fn test_resolve_call_php_interface_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
        "K.php",
        "<?php\ninterface A { function save(); }\nclass K implements A {\n  function audit() {}\n  function save() { $this->audit(); }\n  function run() { $this->save(); }\n}\n",
    );
}

/// Ruby has no interfaces: a module declared first and included stands in.
#[test]
fn test_resolve_call_ruby_module_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::ruby::parser::RubyProvider::new().unwrap(),
        "k.rb",
        "module A\n  def save\n  end\nend\nclass K\n  include A\n  def audit\n  end\n  def save\n    audit\n  end\n  def run\n    save\n    self.save\n  end\nend\n",
    );
}

#[test]
fn test_resolve_call_swift_protocol_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::swift::parser::SwiftProvider::new().unwrap(),
        "K.swift",
        "protocol A { func save() }\nclass K: A {\n  func audit() {}\n  func save() { audit() }\n  func run() { self.save(); save() }\n}\n",
    );
}

#[test]
fn test_resolve_call_dart_abstract_class_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::dart::parser::DartProvider::new().unwrap(),
        "k.dart",
        "abstract class A { void save(); }\nclass K implements A {\n  void audit() {}\n  void save() { audit(); }\n  void run() { this.save(); save(); }\n}\n",
    );
}

/// C++ has no interfaces: an abstract base declared first stands in.
#[test]
fn test_resolve_call_cpp_abstract_base_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        "k.cpp",
        "class A { public: virtual void save() = 0; };\nclass K : public A {\n public:\n  void audit() {}\n  void save() override { audit(); }\n  void run() { this->save(); save(); }\n};\n",
    );
}

/// Go already binds the receiver's method: an interface method is not a
/// same-named node in the file. The fixture guards that.
#[test]
fn test_resolve_call_go_interface_declared_first_binds_own_method() {
    assert_run_binds_own_save(
        &ecp_analyzer::go::parser::GoProvider::new().unwrap(),
        "k.go",
        "package demo\ntype A interface { save() }\ntype K struct{}\nfunc (k *K) audit() {}\nfunc (k *K) save() { k.audit() }\nfunc (k *K) run() { k.save() }\n",
    );
}
