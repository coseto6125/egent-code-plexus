//! Same-name members of one type (overloads, accessor pairs, redefinitions)
//! across the 14 mainstream languages. The uid `(kind, path, owner, name)`
//! has no parameter signature, so every such member shares one identity.
//! Each must still be a named node of its type. A call binds the first by
//! name, and the first references every later one, so upstream `impact`
//! from a later overload's callee reaches the callers of the set.
//!
//! Each fixture declares type `K` with `audit`, two `save` members, and
//! `run`, which calls `save`; only the later `save` calls `audit`.

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

fn named(graph: &ZeroCopyGraph, name: &str) -> Vec<u32> {
    (0..graph.nodes.len() as u32)
        .filter(|&i| graph.nodes[i as usize].name.resolve(&graph.string_pool) == name)
        .collect()
}

fn edges(graph: &ZeroCopyGraph, rel: RelType, source: u32, target: u32) -> Vec<String> {
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == rel && e.source == source && e.target == target)
        .map(|e| e.reason.resolve(&graph.string_pool).to_owned())
        .collect()
}

fn assert_overload_set(provider: &dyn LanguageProvider, path: &str, source: &str) {
    let graph = build(provider, path, source);
    let pool = &graph.string_pool;
    let node = |i: u32| &graph.nodes[i as usize];
    // Rust's trait declaration is a `save` member of `A`.
    let saves: Vec<u32> = named(&graph, "save")
        .into_iter()
        .filter(|&i| node(i).owner_class.resolve(pool) == "K")
        .collect();
    assert_eq!(saves.len(), 2, "{path}: both `save` members are nodes");
    let (first, later) = (saves[0], saves[1]);
    assert_ne!(node(first).uid, node(later).uid, "{path}: distinct uids");
    for &save in &saves {
        assert!(
            ecp_core::uid::keeps_overloads(node(save).kind),
            "{path}: `save` is a method"
        );
    }
    assert!(
        !graph
            .nodes
            .iter()
            .any(|n| { ecp_core::uid::keeps_overloads(n.kind) && n.name.resolve(pool).is_empty() }),
        "{path}: no method is left nameless"
    );

    let ty = *named(&graph, "K")
        .iter()
        .find(|&&i| !ecp_core::uid::keeps_overloads(node(i).kind))
        .expect("type K");
    for &save in &saves {
        assert_eq!(
            edges(&graph, RelType::HasMethod, ty, save).len(),
            1,
            "{path}: K has each `save` once"
        );
    }

    // The upstream path of `audit`: later `save` ← first `save` ← `run`.
    let audit = named(&graph, "audit")[0];
    assert_eq!(
        edges(&graph, RelType::Calls, later, audit).len(),
        1,
        "{path}: the later `save` calls `audit`"
    );
    assert_eq!(
        edges(&graph, RelType::References, first, later),
        ["overload_set"],
        "{path}: the first `save` references the later one"
    );
    let run = named(&graph, "run")[0];
    assert_eq!(
        edges(&graph, RelType::Calls, run, first),
        ["call"],
        "{path}: `run` binds the first `save`"
    );
    assert!(
        edges(&graph, RelType::Calls, run, later).is_empty(),
        "{path}: one Calls edge per call site"
    );
}

#[test]
fn test_overload_set_java_overloads_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        "K.java",
        "class K {\n void audit() {}\n void save(int id) {}\n void save(String name) { audit(); }\n void run() { save(1); }\n}\n",
    );
}

#[test]
fn test_overload_set_csharp_overloads_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        "K.cs",
        "class K {\n void audit() {}\n void save(int id) {}\n void save(string name) { audit(); }\n void run() { save(1); }\n}\n",
    );
}

#[test]
fn test_overload_set_kotlin_overloads_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        "K.kt",
        "class K {\n fun audit() {}\n fun save(id: Int) {}\n fun save(name: String) { audit() }\n fun run() { save(1) }\n}\n",
    );
}

#[test]
fn test_overload_set_typescript_duplicate_methods_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        "k.ts",
        "class K {\n audit() {}\n save(id: number) {}\n save(name: string) { this.audit(); }\n run() { this.save(1); }\n}\n",
    );
}

#[test]
fn test_overload_set_javascript_duplicate_methods_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        "k.js",
        "class K {\n audit() {}\n save(id) {}\n save(id, name) { this.audit(); }\n run() { this.save(1); }\n}\n",
    );
}

#[test]
fn test_overload_set_python_redefined_methods_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::python::PythonProvider::new().unwrap(),
        "k.py",
        "class K:\n    def audit(self):\n        pass\n    def save(self, id):\n        pass\n    def save(self, id, name):\n        self.audit()\n    def run(self):\n        self.save(1)\n",
    );
}

#[test]
fn test_overload_set_go_duplicate_methods_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::go::parser::GoProvider::new().unwrap(),
        "k.go",
        "package demo\ntype K struct{}\nfunc (k *K) audit() {}\nfunc (k *K) save(id int) {}\nfunc (k *K) save(name string) { k.audit() }\nfunc (k *K) run() { k.save(1) }\n",
    );
}

/// Stands for: an inherent `save` and a trait-impl `save` of one type sit in
/// two `Impl` containers, so they are two entities (design_uid §2.5), not
/// one overload set. Rust resolves `self.save()` to the inherent method, so
/// `run` calls it, and no `overload_set` edge leads to the trait one.
#[test]
fn test_overload_set_rust_inherent_and_trait_impl_two_entities_no_overload_set() {
    let graph = build(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        "k.rs",
        "struct K;\nimpl K {\n fn audit(&self) {}\n fn save(&self) {}\n fn run(&self) { self.save(); }\n}\nimpl A for K { fn save(&self) { self.audit(); } }\ntrait A { fn save(&self); }\n",
    );
    let pool = &graph.string_pool;
    let node = |i: u32| &graph.nodes[i as usize];
    let saves: Vec<u32> = named(&graph, "save")
        .into_iter()
        .filter(|&i| node(i).owner_class.resolve(pool) == "K")
        .collect();
    assert_eq!(saves.len(), 2, "both `save` members are nodes");
    let (inherent, trait_impl) = (saves[0], saves[1]);
    assert_ne!(node(inherent).uid, node(trait_impl).uid, "distinct uids");
    let ty = *named(&graph, "K")
        .iter()
        .find(|&&i| !ecp_core::uid::keeps_overloads(node(i).kind))
        .expect("type K");
    for &save in &saves {
        assert_eq!(
            edges(&graph, RelType::HasMethod, ty, save).len(),
            1,
            "K has each `save` once"
        );
    }
    assert!(
        edges(&graph, RelType::References, inherent, trait_impl).is_empty()
            && edges(&graph, RelType::References, trait_impl, inherent).is_empty(),
        "no overload_set between the two impls"
    );
    let run = named(&graph, "run")[0];
    assert_eq!(edges(&graph, RelType::Calls, run, inherent), ["call"]);
    assert!(edges(&graph, RelType::Calls, run, trait_impl).is_empty());
}

#[test]
fn test_overload_set_php_duplicate_methods_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
        "K.php",
        "<?php\nclass K {\n function audit() {}\n function save($id) {}\n function save($id, $name) { $this->audit(); }\n function run() { $this->save(1); }\n}\n",
    );
}

#[test]
fn test_overload_set_ruby_redefined_methods_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::ruby::parser::RubyProvider::new().unwrap(),
        "k.rb",
        "class K\n  def audit\n  end\n  def save(id)\n  end\n  def save(id, name)\n    audit()\n  end\n  def run\n    save(1)\n  end\nend\n",
    );
}

#[test]
fn test_overload_set_swift_overloads_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::swift::parser::SwiftProvider::new().unwrap(),
        "K.swift",
        "class K {\n func audit() {}\n func save(id: Int) {}\n func save(name: String) { audit() }\n func run() { save(id: 1) }\n}\n",
    );
}

#[test]
fn test_overload_set_cpp_overloads_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        "k.cpp",
        "class K {\n void audit() {}\n void save(int id) {}\n void save(const char* name) { audit(); }\n void run() { save(1); }\n};\n",
    );
}

#[test]
fn test_overload_set_dart_duplicate_methods_both_named_and_called() {
    assert_overload_set(
        &ecp_analyzer::dart::parser::DartProvider::new().unwrap(),
        "k.dart",
        "class K {\n void audit() {}\n void save(int id) {}\n void save(String name) { audit(); }\n void run() { save(1); }\n}\n",
    );
}

/// Stands for: C has no methods, and a function redefined under `#ifdef`
/// is a redefinition that keeps a node of its own (design uid §2.3): two
/// `save` nodes with distinct uids, `run`'s call binds the first, the
/// first's `overload_set` reference reaches the second, and no
/// `ifdef-redef` BlindSpot reports it, since nothing merged. Before the
/// change the second was a tombstone reported as `ifdef-redef`.
#[test]
fn test_overload_set_c_ifdef_redefinition_two_nodes_in_one_set() {
    let graph = build(
        &ecp_analyzer::c::parser::CProvider::new().unwrap(),
        "k.c",
        "#ifdef A\nint save(int id) { return 0; }\n#else\nint save(int id) { return 1; }\n#endif\nint run(void) { return save(1); }\n",
    );
    let pool = &graph.string_pool;
    let saves = named(&graph, "save");
    assert_eq!(saves.len(), 2, "two `save` nodes");
    let (first, later) = (saves[0], saves[1]);
    assert_ne!(
        graph.nodes[first as usize].uid, graph.nodes[later as usize].uid,
        "distinct uids"
    );
    let run = named(&graph, "run")[0];
    assert_eq!(edges(&graph, RelType::Calls, run, first), ["call"]);
    assert!(edges(&graph, RelType::Calls, run, later).is_empty());
    assert_eq!(
        edges(&graph, RelType::References, first, later),
        ["overload_set"],
        "the first `save` references the second"
    );
    assert!(
        !graph
            .blind_spots
            .iter()
            .any(|b| b.kind.resolve(pool) == "ifdef-redef"),
        "a kept redefinition reports nothing"
    );
    for save in saves {
        assert_eq!(
            graph.nodes[save as usize].kind,
            NodeKind::Function,
            "C `save` is a function"
        );
    }
}
