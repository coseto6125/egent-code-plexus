//! A module- or type-qualified function passed as a value
//! (`v.iter_mut().for_each(parse::migrate)`, `.map(Entry::new)`) gets a
//! `References` edge with reason `function_reference`, like the bare
//! `for_each(migrate)` form. The qualifier picks the declaration: with
//! `a::run` and `b::run` in the tree, `for_each(a::run)` binds only `a`.
//!
//! The edge is never a `Calls` edge, and a path that is not a function value
//! records nothing: a callee, an import, a macro path, an enum variant, a
//! constant, an external associated function.
//!
//! Contract of the 13 other-language tests: the Rust-only spec field
//! (`NameRefSpec::qualified_values`) leaves their bare-name edge unchanged.
use crate::name_ref_support;

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use name_ref_support::{
    assert_one_cross_file, calls, function_refs, graph, r, value_reads, FUNCTION_REFERENCE,
    SAME_FILE,
};

fn none() -> Vec<name_ref_support::Ref> {
    Vec::new()
}

fn rust_graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph(&RustProvider::new().unwrap(), files)
}

/// Every `function_reference` edge in the graph, whatever its source.
fn all_function_reference_edges(g: &ZeroCopyGraph) -> usize {
    let pool = g.string_pool.as_slice();
    g.edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::References && e.reason.resolve(pool) == FUNCTION_REFERENCE
        })
        .count()
}

const LIB_AB: &str = "pub mod a;\npub mod b;\n";
const RUN: &str = "pub fn run(_: &u32) {}\n";

// ---- Rust: module-qualified function as a value ----

#[test]
fn test_function_reference_rust_module_path_argument_returns_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod parse;\npub mod cache;\n"),
        ("src/parse.rs", "pub fn migrate(_: &mut u32) {}\n"),
        (
            "src/cache.rs",
            "pub fn all(v: &mut Vec<u32>) {\n    v.iter_mut().for_each(crate::parse::migrate);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "all"), "src/parse.rs", "migrate");
    assert_eq!(calls(&g, "all"), Vec::<(String, String)>::new());
}

#[test]
fn test_function_reference_rust_plain_module_path_argument_returns_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod parse;\npub fn all(v: &mut Vec<u32>) {\n    v.iter_mut().for_each(parse::migrate);\n}\n"),
        ("src/parse.rs", "pub fn migrate(_: &mut u32) {}\n"),
    ]);
    assert_one_cross_file(&function_refs(&g, "all"), "src/parse.rs", "migrate");
}

#[test]
fn test_function_reference_rust_super_prefixed_path_returns_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod util;\npub mod child;\n"),
        ("src/util.rs", "pub fn f(_: &u32) {}\n"),
        (
            "src/child.rs",
            "pub fn all(v: &Vec<u32>) {\n    v.iter().for_each(super::util::f);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "all"), "src/util.rs", "f");
}

/// A path whose last qualifier is `super`, `crate` or `self` has no module
/// name to resolve through, so it records nothing. Honest no-data until the
/// `NameRef` can carry a `::` path; the contract is "no wrong binding".
#[test]
fn test_function_reference_rust_keyword_only_qualifier_returns_no_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod child;\npub fn f(_: &u32) {}\n"),
        (
            "src/child.rs",
            "pub fn all(v: &Vec<u32>) {\n    v.iter().for_each(super::f);\n    v.iter().for_each(crate::f);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "all"), none());
}

#[test]
fn test_function_reference_rust_nested_module_path_returns_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod a;\npub mod user;\n"),
        ("src/a/mod.rs", "pub mod b;\n"),
        ("src/a/b.rs", "pub fn f(_: &u32) {}\n"),
        (
            "src/user.rs",
            "pub fn all(v: &Vec<u32>) {\n    v.iter().for_each(crate::a::b::f);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "all"), "src/a/b.rs", "f");
}

#[test]
fn test_function_reference_rust_same_named_modules_binds_only_named_module() {
    let g = rust_graph(&[
        ("src/lib.rs", LIB_AB),
        ("src/a.rs", RUN),
        ("src/b.rs", RUN),
        (
            "src/user.rs",
            "pub fn go(v: &Vec<u32>) {\n    v.iter().for_each(a::run);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "go"), "src/a.rs", "run");
}

#[test]
fn test_function_reference_rust_same_named_modules_second_module_returns_its_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", LIB_AB),
        ("src/a.rs", RUN),
        ("src/b.rs", RUN),
        (
            "src/user.rs",
            "pub fn go(v: &Vec<u32>) {\n    v.iter().for_each(b::run);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "go"), "src/b.rs", "run");
}

#[test]
fn test_function_reference_rust_let_binding_returns_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod m;\n"),
        ("src/m.rs", "pub fn f() {}\n"),
        (
            "src/user.rs",
            "pub fn go() {\n    let g = m::f;\n    g();\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "go"), "src/m.rs", "f");
}

#[test]
fn test_function_reference_rust_return_value_returns_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod m;\n"),
        ("src/m.rs", "pub fn f() {}\n"),
        ("src/user.rs", "pub fn pick() -> fn() {\n    m::f\n}\n"),
    ]);
    assert_one_cross_file(&function_refs(&g, "pick"), "src/m.rs", "f");
}

#[test]
fn test_function_reference_rust_struct_literal_field_returns_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod m;\n"),
        ("src/m.rs", "pub fn f() {}\n"),
        (
            "src/user.rs",
            "pub struct S {\n    pub cb: fn(),\n}\n\npub fn make() -> S {\n    S { cb: m::f }\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "make"), "src/m.rs", "f");
}

#[test]
fn test_function_reference_rust_array_element_returns_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod m;\n"),
        ("src/m.rs", "pub fn f() {}\n"),
        (
            "src/user.rs",
            "pub fn all() -> [fn(); 1] {\n    [m::f]\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "all"), "src/m.rs", "f");
}

// ---- Rust: associated functions ----

#[test]
fn test_function_reference_rust_associated_function_returns_edge() {
    let g = rust_graph(&[(
        "src/lib.rs",
        "pub struct Entry(u32);\n\nimpl Entry {\n    pub fn new(n: u32) -> Entry {\n        Entry(n)\n    }\n}\n\npub fn build(v: Vec<u32>) -> Vec<Entry> {\n    v.into_iter().map(Entry::new).collect()\n}\n",
    )]);
    assert_one_cross_file(&function_refs(&g, "build"), "src/lib.rs", "new");
}

/// `String::from` is external: no project `from` binds, even a unique one.
#[test]
fn test_function_reference_rust_external_associated_function_returns_no_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod other;\n"),
        ("src/other.rs", "pub fn from() {}\n"),
        (
            "src/user.rs",
            "pub fn conv(v: Vec<&str>) -> Vec<String> {\n    v.into_iter().map(String::from).collect()\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "conv"), none());
}

// ---- Rust: positions that are no function value ----

/// The callee of a call is a `Calls` edge only.
#[test]
fn test_function_reference_rust_qualified_callee_returns_only_calls_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod m;\n"),
        ("src/m.rs", "pub fn f() {}\n"),
        ("src/user.rs", "pub fn go() {\n    m::f();\n}\n"),
    ]);
    assert_eq!(
        calls(&g, "go"),
        vec![("src/m.rs".to_string(), "f".to_string())]
    );
    assert_eq!(function_refs(&g, "go"), none());
}

#[test]
fn test_function_reference_rust_variant_constructor_and_const_return_no_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod other;\n"),
        ("src/other.rs", "pub fn A() {}\npub fn MAX() {}\n"),
        (
            "src/user.rs",
            "pub enum Kind {\n    A,\n    B,\n}\n\npub fn wrap(x: u32) -> Option<u32> {\n    Some(x)\n}\n\npub fn kind() -> Kind {\n    Kind::A\n}\n\npub fn top() -> u32 {\n    u32::MAX\n}\n",
        ),
    ]);
    for reader in ["wrap", "kind", "top"] {
        assert_eq!(function_refs(&g, reader), none(), "{reader}");
        assert_eq!(value_reads(&g, reader), none(), "{reader}");
    }
}

/// A `use` declaration names the function without using it.
#[test]
fn test_function_reference_rust_use_declaration_returns_no_reference_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod m;\n"),
        ("src/m.rs", "pub fn f() {}\n"),
        (
            "src/user.rs",
            "use crate::m::f;\n\npub fn go() {\n    f();\n}\n",
        ),
    ]);
    assert_eq!(all_function_reference_edges(&g), 0);
}

/// A path in a type position (`m::T`) is no value.
#[test]
fn test_function_reference_rust_type_position_path_returns_no_edge() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod m;\n"),
        ("src/m.rs", "pub struct T;\npub fn T() {}\n"),
        (
            "src/user.rs",
            "pub fn go(_x: m::T) -> Option<m::T> {\n    let y: Option<m::T> = None;\n    y\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "go"), none());
}

/// Pins the macro behaviour. Inside a macro argument list the tokens are not
/// parsed into a `scoped_identifier`, so the qualified spec records nothing
/// and the bare-name rule sees the tokens `m`, `::`, `f`: the bare `f` may
/// bind at the global tier, exactly as before this change. The contract is
/// "at most one edge, and only to the named function": no new qualified edge
/// comes from inside a macro.
#[test]
fn test_function_reference_rust_macro_argument_binds_at_most_the_named_function() {
    let g = rust_graph(&[
        ("src/lib.rs", "pub mod m;\n"),
        ("src/m.rs", "pub fn f() {}\n"),
        (
            "src/user.rs",
            "pub fn wrap() -> Vec<fn()> {\n    vec![m::f]\n}\n",
        ),
    ]);
    let edges = function_refs(&g, "wrap");
    assert!(edges.len() <= 1, "{edges:?}");
    if !edges.is_empty() {
        assert_one_cross_file(&edges, "src/m.rs", "f");
    }
}

// ---- Other languages: the bare-name edge is unchanged ----

fn assert_same_file_ref(
    provider: &dyn LanguageProvider,
    file: &str,
    src: &str,
    registrar: &str,
    target: &str,
) {
    let g = graph(provider, &[(file, src)]);
    assert_eq!(
        function_refs(&g, registrar),
        vec![r(file, target, FUNCTION_REFERENCE, SAME_FILE)],
        "{file}: {src}"
    );
}

#[test]
fn test_function_reference_typescript_bare_argument_unchanged_returns_edge() {
    assert_same_file_ref(
        &TypeScriptProvider::new().unwrap(),
        "app.ts",
        "function handler(): void {}\nexport function wire(el: any): void {\n  el.addEventListener('click', handler);\n}\n",
        "wire",
        "handler",
    );
}

#[test]
fn test_function_reference_javascript_bare_argument_unchanged_returns_edge() {
    assert_same_file_ref(
        &JavaScriptProvider::new().unwrap(),
        "app.js",
        "function tick() {}\nexport function start() {\n  setTimeout(tick, 10);\n}\n",
        "start",
        "tick",
    );
}

#[test]
fn test_function_reference_python_bare_argument_unchanged_returns_edge() {
    assert_same_file_ref(
        &PythonProvider::new().unwrap(),
        "app.py",
        "import threading\n\ndef worker():\n    pass\n\ndef start():\n    threading.Thread(target=worker).start()\n",
        "start",
        "worker",
    );
}

#[test]
fn test_function_reference_java_method_reference_unchanged_returns_edge() {
    assert_same_file_ref(
        &JavaProvider::new().unwrap(),
        "P.java",
        "class P {\n    void print(String s) {}\n    void run(java.util.List<String> xs) {\n        xs.forEach(this::print);\n    }\n}\n",
        "run",
        "print",
    );
}

#[test]
fn test_function_reference_kotlin_callable_reference_unchanged_returns_edge() {
    assert_same_file_ref(
        &KotlinProvider::new().unwrap(),
        "k.kt",
        "fun transform(x: Int): Int = x\n\nfun apply(xs: List<Int>): List<Int> {\n    return xs.map(::transform)\n}\n",
        "apply",
        "transform",
    );
}

#[test]
fn test_function_reference_csharp_method_group_unchanged_returns_edge() {
    assert_same_file_ref(
        &CSharpProvider::new().unwrap(),
        "P.cs",
        "class P {\n    void Print(string s) {}\n    void Run(System.Collections.Generic.List<string> xs) {\n        xs.ForEach(Print);\n    }\n}\n",
        "Run",
        "Print",
    );
}

#[test]
fn test_function_reference_go_bare_argument_unchanged_returns_edge() {
    assert_same_file_ref(
        &GoProvider::new().unwrap(),
        "main.go",
        "package main\n\nimport \"net/http\"\n\nfunc handler(w http.ResponseWriter, r *http.Request) {}\n\nfunc wire() {\n\thttp.HandleFunc(\"/\", handler)\n}\n",
        "wire",
        "handler",
    );
}

#[test]
fn test_function_reference_php_string_callable_unchanged_returns_edge() {
    assert_same_file_ref(
        &PhpProvider::new().unwrap(),
        "p.php",
        "<?php\nfunction fmt($x) { return $x; }\n\nfunction apply($xs) {\n    return array_map('fmt', $xs);\n}\n",
        "apply",
        "fmt",
    );
}

#[test]
fn test_function_reference_ruby_method_symbol_unchanged_returns_edge() {
    assert_same_file_ref(
        &RubyProvider::new().unwrap(),
        "svc.rb",
        "class Svc\n  def handler(x)\n    x\n  end\n\n  def wire(items)\n    items.each(&method(:handler))\n  end\nend\n",
        "wire",
        "handler",
    );
}

#[test]
fn test_function_reference_swift_bare_argument_unchanged_returns_edge() {
    assert_same_file_ref(
        &SwiftProvider::new().unwrap(),
        "s.swift",
        "func transform(_ x: Int) -> Int {\n    return x\n}\n\nfunc apply(_ xs: [Int]) -> [Int] {\n    return xs.map(transform)\n}\n",
        "apply",
        "transform",
    );
}

#[test]
fn test_function_reference_c_qsort_comparator_unchanged_returns_edge() {
    assert_same_file_ref(
        &CProvider::new().unwrap(),
        "s.c",
        "#include <stdlib.h>\n\nint cmp(const void *a, const void *b) {\n    return 0;\n}\n\nvoid sort_all(int *xs, int n) {\n    qsort(xs, n, sizeof(int), cmp);\n}\n",
        "sort_all",
        "cmp",
    );
}

#[test]
fn test_function_reference_cpp_sort_comparator_unchanged_returns_edge() {
    assert_same_file_ref(
        &CppProvider::new().unwrap(),
        "s.cpp",
        "#include <algorithm>\n#include <vector>\n\nbool byValue(int a, int b) {\n    return a < b;\n}\n\nvoid sortAll(std::vector<int>& xs) {\n    std::sort(xs.begin(), xs.end(), byValue);\n}\n",
        "sortAll",
        "byValue",
    );
}

#[test]
fn test_function_reference_dart_bare_argument_unchanged_returns_edge() {
    assert_same_file_ref(
        &DartProvider::new().unwrap(),
        "d.dart",
        "int transform(int x) {\n  return x;\n}\n\nList<int> apply(List<int> xs) {\n  return xs.map(transform).toList();\n}\n",
        "apply",
        "transform",
    );
}
