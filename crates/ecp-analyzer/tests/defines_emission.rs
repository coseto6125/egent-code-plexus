//! `Defines` edge emission — spec §8.4 of Sub-projects 1/5.
//!
//! Tests the scope-containment fill for File / Namespace / Module:
//!   - File → top-level symbol (owner_class.is_none())
//!   - Namespace → child (owner_class == Some(namespace_name))
//!   - Module → child (owner_class == Some(module_name))
//!
//! Also includes two regression tests:
//!   - Class → Method emits HasMethod ONLY (Defines must not appear)
//!   - owner_class gate: top-level Class gets File→Class Defines; its methods do not

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};

// ── helpers ─────────────────────────────────────────────────────────────────

fn build(graphs: Vec<LocalGraph>) -> ZeroCopyGraph {
    let mut b = GraphBuilder::new();
    for g in graphs {
        b.add_graph(g);
    }
    b.build()
}

fn parse_file<P: LanguageProvider>(provider: &P, path: &str, src: &str) -> LocalGraph {
    provider
        .parse_file(path.as_ref(), src.as_bytes())
        .expect("parse_file")
}

/// Returns all Defines edges in `graph` as `(source_name, target_name)` pairs.
fn defines_pairs(graph: &ZeroCopyGraph) -> Vec<(String, String)> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Defines)
        .map(|e| {
            let src = graph.nodes[e.source as usize]
                .name
                .resolve(pool)
                .to_string();
            let tgt = graph.nodes[e.target as usize]
                .name
                .resolve(pool)
                .to_string();
            (src, tgt)
        })
        .collect()
}

fn count_edges_of_type(graph: &ZeroCopyGraph, rel: RelType) -> usize {
    graph.edges.iter().filter(|e| e.rel_type == rel).count()
}

// ── 1. Namespace + Class — C# ─────────────────────────────────────────────────
//
// `stamp_owner_class_by_span` now treats Namespace as a container kind, so
// the enclosed `class Foo` carries `owner_class = Some("App.Api")` and
// `scope_defines::Pass2` emits `Namespace(App.Api) → Class(Foo)` instead of
// `File → Foo`. The no-duplication invariant from `scope_defines.rs:9-11`
// then keeps `File → Foo` suppressed (members of a Namespace/Module are
// covered by their container, not the file).

#[test]
fn csharp_namespace_and_enclosed_class() {
    let provider = ecp_analyzer::c_sharp::parser::CSharpProvider::new().expect("csharp provider");
    let src = r#"
namespace App.Api {
    public class Foo {}
}
"#;
    let graph = build(vec![parse_file(&provider, "Api.cs", src)]);
    let pairs = defines_pairs(&graph);
    // File (Api.cs) → Namespace (App.Api)
    assert!(
        pairs.iter().any(|(s, t)| s == "Api.cs" && t == "App.Api"),
        "expected File->Defines->Namespace(App.Api); got: {pairs:?}"
    );
    // Namespace (App.Api) → Class (Foo) — FU-016 Pass2 emission
    assert!(
        pairs.iter().any(|(s, t)| s == "App.Api" && t == "Foo"),
        "expected Namespace(App.Api)->Defines->Class(Foo); got: {pairs:?}"
    );
    // No duplicate File->Foo edge — Foo is owned by the namespace.
    assert!(
        !pairs.iter().any(|(s, t)| s == "Api.cs" && t == "Foo"),
        "File->Foo should NOT be emitted when Foo is namespaced; got: {pairs:?}"
    );
}

// ── 2. Namespace-level Function — PHP ─────────────────────────────────────────
//
// PHP `namespace App; function bar() {}` emits bar with owner_class=None
// (namespace containers don't set owner_class in stamp_owner_class_by_span).
// File → bar Defines fires via the owner_class=None gate.

#[test]
fn php_namespace_function() {
    let provider = ecp_analyzer::php::parser::PhpProvider::new().expect("php provider");
    let src = r#"<?php
namespace App;
function bar() {}
"#;
    let graph = build(vec![parse_file(&provider, "bar.php", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "bar"),
        "expected Defines edge targeting function `bar`; got: {pairs:?}"
    );
}

// ── 3. Module + nested Function — Rust ───────────────────────────────────────
//
// `mod foo { pub fn bar() {} }` — FU-016 added a `stamp_owner_class_by_span`
// pass to the Rust parser tail, so `bar` carries `owner_class = Some("foo")`
// and `scope_defines::Pass2` emits `Module(foo) → Function(bar)` instead of
// `File → bar`. `impl Foo` ownership set earlier in the parser is preserved
// via the `owner_class.is_none()` guard in `framework_helpers`.

#[test]
fn rust_module_and_nested_function() {
    let provider = ecp_analyzer::rust::parser::RustProvider::new().expect("rust provider");
    let src = r#"
mod foo {
    pub fn bar() {}
}
"#;
    let graph = build(vec![parse_file(&provider, "lib.rs", src)]);
    let pairs = defines_pairs(&graph);
    // File → Module(foo)
    assert!(
        pairs.iter().any(|(s, t)| s == "lib.rs" && t == "foo"),
        "expected File->Defines->Module(foo); got: {pairs:?}"
    );
    // Module(foo) → Function(bar) — FU-016 Pass2 emission
    assert!(
        pairs.iter().any(|(s, t)| s == "foo" && t == "bar"),
        "expected Module(foo)->Defines->Function(bar); got: {pairs:?}"
    );
    // No duplicate File->bar edge — bar is owned by the module.
    assert!(
        !pairs.iter().any(|(s, t)| s == "lib.rs" && t == "bar"),
        "File->bar should NOT be emitted when bar is module-owned; got: {pairs:?}"
    );
}

// ── 4. Module → Class — Python ───────────────────────────────────────────────

#[test]
fn python_module_level_class() {
    let provider = ecp_analyzer::python::parser::PythonProvider::new().expect("python provider");
    let src = r#"
class MyModel:
    pass
"#;
    let graph = build(vec![parse_file(&provider, "app/__init__.py", src)]);
    // top-level class → File → Class Defines
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "MyModel"),
        "expected File->Defines->Class(MyModel); got: {pairs:?}"
    );
}

// ── 5. File → top-level Function — Python ────────────────────────────────────

#[test]
fn python_file_to_top_level_function() {
    let provider = ecp_analyzer::python::parser::PythonProvider::new().expect("python provider");
    let src = "def compute(x):\n    return x * 2\n";
    let graph = build(vec![parse_file(&provider, "utils.py", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "compute"),
        "expected File->Defines->Function(compute); got: {pairs:?}"
    );
}

// ── 6. File → top-level Const — TypeScript ───────────────────────────────────

#[test]
fn typescript_file_to_const() {
    let provider =
        ecp_analyzer::typescript::parser::TypeScriptProvider::new().expect("ts provider");
    let src = "export const MAX_RETRIES = 3;\n";
    let graph = build(vec![parse_file(&provider, "config.ts", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "MAX_RETRIES"),
        "expected File->Defines->Const(MAX_RETRIES); got: {pairs:?}"
    );
}

// ── 7. File → top-level Function — JavaScript ────────────────────────────────

#[test]
fn javascript_file_to_function() {
    let provider =
        ecp_analyzer::javascript::parser::JavaScriptProvider::new().expect("js provider");
    let src = "function greet(name) { return 'hello ' + name; }\n";
    let graph = build(vec![parse_file(&provider, "greet.js", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "greet"),
        "expected File->Defines->Function(greet); got: {pairs:?}"
    );
}

// ── 8. File → top-level Class — Java ─────────────────────────────────────────

#[test]
fn java_file_to_class() {
    let provider = ecp_analyzer::java::parser::JavaProvider::new().expect("java provider");
    let src = "public class Foo { public void run() {} }\n";
    let graph = build(vec![parse_file(&provider, "Foo.java", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "Foo"),
        "expected File->Defines->Class(Foo); got: {pairs:?}"
    );
}

// ── 9. File → top-level Class — Kotlin ───────────────────────────────────────

#[test]
fn kotlin_file_to_class() {
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().expect("kotlin provider");
    let src = "class Greeter(val name: String) { fun greet() = println(name) }\n";
    let graph = build(vec![parse_file(&provider, "Greeter.kt", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "Greeter"),
        "expected File->Defines->Class(Greeter); got: {pairs:?}"
    );
}

// ── 10. File → top-level Function — Go ───────────────────────────────────────

#[test]
fn go_file_to_function() {
    let provider = ecp_analyzer::go::parser::GoProvider::new().expect("go provider");
    let src = "package main\nfunc HandleRequest(w http.ResponseWriter, r *http.Request) {}\n";
    let graph = build(vec![parse_file(&provider, "handler.go", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "HandleRequest"),
        "expected File->Defines->Function(HandleRequest); got: {pairs:?}"
    );
}

// ── 11. File → top-level Function — Ruby ─────────────────────────────────────

#[test]
fn ruby_file_to_function() {
    let provider = ecp_analyzer::ruby::parser::RubyProvider::new().expect("ruby provider");
    let src = "def process_order(order)\n  order.save\nend\n";
    let graph = build(vec![parse_file(&provider, "orders.rb", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "process_order"),
        "expected File->Defines->Function(process_order); got: {pairs:?}"
    );
}

// ── 12. File → top-level Function — Swift ────────────────────────────────────

#[test]
fn swift_file_to_function() {
    let provider = ecp_analyzer::swift::parser::SwiftProvider::new().expect("swift provider");
    let src = "func fetchData(from url: URL) -> Data? { return nil }\n";
    let graph = build(vec![parse_file(&provider, "network.swift", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "fetchData"),
        "expected File->Defines->Function(fetchData); got: {pairs:?}"
    );
}

// ── 13. File → top-level Function — C ────────────────────────────────────────

#[test]
fn c_file_to_function() {
    let provider = ecp_analyzer::c::parser::CProvider::new().expect("c provider");
    let src = "int compute_sum(int a, int b) { return a + b; }\n";
    let graph = build(vec![parse_file(&provider, "math.c", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "compute_sum"),
        "expected File->Defines->Function(compute_sum); got: {pairs:?}"
    );
}

// ── 14. File → top-level Class — C++ ─────────────────────────────────────────

#[test]
fn cpp_file_to_class() {
    let provider = ecp_analyzer::cpp::parser::CppProvider::new().expect("cpp provider");
    let src = "class Vector { public: int x; int y; };\n";
    let graph = build(vec![parse_file(&provider, "vector.cpp", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "Vector"),
        "expected File->Defines->Class(Vector); got: {pairs:?}"
    );
}

// ── 15. File → top-level Class — Dart ────────────────────────────────────────

#[test]
fn dart_file_to_class() {
    let provider = ecp_analyzer::dart::parser::DartProvider::new().expect("dart provider");
    let src = "class Repository { void save(Object item) {} }\n";
    let graph = build(vec![parse_file(&provider, "repo.dart", src)]);
    let pairs = defines_pairs(&graph);
    assert!(
        pairs.iter().any(|(_, t)| t == "Repository"),
        "expected File->Defines->Class(Repository); got: {pairs:?}"
    );
}

// ── 16. Regression: Class → Method emits HasMethod ONLY, not Defines ─────────

#[test]
fn no_duplication_class_method() {
    // Java: `class Foo { void bar() {} }` — bar's owner_class == Some("Foo").
    // After build(), the Foo→bar edge must be HasMethod, not Defines.
    let provider = ecp_analyzer::java::parser::JavaProvider::new().expect("java provider");
    let src = "public class Foo { public void bar() {} }\n";
    let graph = build(vec![parse_file(&provider, "Foo.java", src)]);

    let pool = graph.string_pool.as_slice();

    // Find node ids for Foo and bar
    let foo_id = graph
        .nodes
        .iter()
        .position(|n| n.name.resolve(pool) == "Foo" && n.kind == NodeKind::Class)
        .expect("Class Foo must exist") as u32;
    let bar_id = graph
        .nodes
        .iter()
        .position(|n| n.name.resolve(pool) == "bar" && n.kind == NodeKind::Method)
        .expect("Method bar must exist") as u32;

    // HasMethod edge must exist
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.source == foo_id && e.target == bar_id && e.rel_type == RelType::HasMethod),
        "expected HasMethod edge Foo->bar"
    );

    // Defines edge must NOT exist between Foo and bar
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.source == foo_id && e.target == bar_id && e.rel_type == RelType::Defines),
        "Defines edge Foo->bar must not be emitted (would duplicate HasMethod)"
    );
}

// ── 17. Regression: owner_class gate — top-level Class gets Defines, members don't ──

#[test]
fn owner_class_none_gate() {
    // Python: `class Foo:\n    def method(self): pass`
    // Foo is top-level (owner_class=None) → File->Foo Defines expected.
    // method's owner_class=Some("Foo") → must NOT get File->method Defines.
    let provider = ecp_analyzer::python::parser::PythonProvider::new().expect("python provider");
    let src = "class Service:\n    def handle(self):\n        pass\n";
    let graph = build(vec![parse_file(&provider, "service.py", src)]);

    let pairs = defines_pairs(&graph);

    // File node name is "service.py"
    let file_has_service = pairs
        .iter()
        .any(|(s, t)| s == "service.py" && t == "Service");
    assert!(
        file_has_service,
        "File->Defines->Class(Service) must be emitted; got: {pairs:?}"
    );

    // handle is a method of Service — File must NOT emit Defines for it
    let file_has_handle = pairs
        .iter()
        .any(|(s, t)| s == "service.py" && t == "handle");
    assert!(
        !file_has_handle,
        "File->Defines->Method(handle) must NOT be emitted (covered by HasMethod); got: {pairs:?}"
    );

    // Sanity: Defines count must be >= 1 (at least Service)
    let defines_count = count_edges_of_type(&graph, RelType::Defines);
    assert!(
        defines_count >= 1,
        "at least 1 Defines edge expected; got {defines_count}"
    );
}
