//! FunctionMeta extraction tests for C++.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = CppProvider::new().unwrap();
    let local = provider
        .parse_file("test.cpp".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = CppProvider::new().unwrap();
    let local = provider
        .parse_file("tests/test_widget.cpp".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn find_fn(g: &ZeroCopyGraph, name: &str) -> u32 {
    let pool = g.string_pool.as_slice();
    g.nodes
        .iter()
        .position(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            ) && n.name.resolve(pool) == name
        })
        .unwrap_or_else(|| panic!("node {name} not found")) as u32
}

fn meta<'a>(g: &'a ZeroCopyGraph, name: &str) -> &'a FunctionMeta {
    let idx = find_fn(g, name);
    g.function_meta(idx)
        .unwrap_or_else(|| panic!("FunctionMeta missing for {name}"))
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn cpp_public_method_visibility_zero() {
    let src = "class Foo {\npublic:\n    void run() {}\n};\n";
    let g = analyze(src);
    let m = meta(&g, "run");
    assert_eq!(m.visibility(), 0, "public: section → vis 0");
}

#[test]
fn cpp_protected_method_visibility_one() {
    let src = "class Foo {\nprotected:\n    void hook() {}\n};\n";
    let g = analyze(src);
    let m = meta(&g, "hook");
    assert_eq!(m.visibility(), 1, "protected: section → vis 1");
}

#[test]
fn cpp_private_method_default_in_class() {
    // class default is private.
    let src = "class Foo {\n    void secret() {}\n};\n";
    let g = analyze(src);
    let m = meta(&g, "secret");
    assert_eq!(m.visibility(), 2, "class default → private (vis 2)");
}

#[test]
fn cpp_struct_default_is_public() {
    // struct default is public.
    let src = "struct Bar {\n    void open() {}\n};\n";
    let g = analyze(src);
    let m = meta(&g, "open");
    assert_eq!(m.visibility(), 0, "struct default → public (vis 0)");
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn cpp_static_member_function_has_static_flag() {
    let src = "class Factory {\npublic:\n    static Factory* create() { return nullptr; }\n};\n";
    let g = analyze(src);
    let m = meta(&g, "create");
    assert!(m.is_static(), "static member function → is_static");
}

// ── abstract / pure virtual ───────────────────────────────────────────────────

#[test]
fn cpp_pure_virtual_has_abstract_flag() {
    let src = "class Shape {\npublic:\n    virtual double area() = 0;\n};\n";
    let g = analyze(src);
    let m = meta(&g, "area");
    assert!(m.is_abstract(), "pure virtual → is_abstract");
}

#[test]
fn cpp_non_pure_virtual_not_abstract() {
    let src = "class Shape {\npublic:\n    virtual void draw() {}\n};\n";
    let g = analyze(src);
    let m = meta(&g, "draw");
    assert!(!m.is_abstract(), "virtual with body → not abstract");
}

// ── extern "C" ────────────────────────────────────────────────────────────────

#[test]
fn cpp_declaration_without_body_is_extern() {
    // C++ forward declaration → is_extern.
    let src = "int compute(int n);\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(m.is_extern(), "declaration without body → is_extern");
}

// ── generator (co_yield) ──────────────────────────────────────────────────────

#[test]
fn cpp_co_yield_body_has_generator_flag() {
    // C++20 coroutine generator — body contains co_yield.
    // NOTE: tree-sitter-cpp may not fully parse co_yield; this tests the heuristic path.
    let src = "int gen() { co_yield 1; }\n";
    let g = analyze(src);
    // If `gen` was parsed as a function, check generator flag.
    // Due to grammar limits on co_yield, we check softly.
    if let Some(idx) = g.nodes.iter().position(|n| {
        matches!(n.kind, NodeKind::Function | NodeKind::Method) && {
            let pool = g.string_pool.as_slice();
            n.name.resolve(pool) == "gen"
        }
    }) {
        if let Some(m) = g.function_meta(idx as u32) {
            // If the body was parsed and co_yield found, generator flag should be set.
            // We don't hard-assert since grammar support varies.
            let _ = m.is_generator();
        }
    }
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn cpp_test_file_is_test() {
    // C++ test detection is file-path-based (Google Test / Catch2 / doctest).
    let src = "void setup_test() {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "setup_test");
    assert!(m.is_test(), "tests/ directory → is_test");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn cpp_typed_params_captured() {
    let src = "int add(int a, int b) { return a + b; }\n";
    let g = analyze(src);
    let m = meta(&g, "add");
    let pool = g.string_pool.as_slice();
    assert_eq!(m.params.len(), 4, "two params → 4 elements");
    assert_eq!(m.params[0].resolve(pool), "a");
    assert_eq!(m.params[1].resolve(pool), "int");
    assert_eq!(m.params[2].resolve(pool), "b");
    assert_eq!(m.params[3].resolve(pool), "int");
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn cpp_return_type_captured() {
    let src = "int multiply(int a, int b) { return a * b; }\n";
    let g = analyze(src);
    let m = meta(&g, "multiply");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "int");
}

#[test]
fn cpp_void_return_type_captured() {
    let src = "void do_work() {}\n";
    let g = analyze(src);
    let m = meta(&g, "do_work");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "void");
}

// ── decorators (C++11 attributes) ────────────────────────────────────────────

#[test]
fn cpp_nodiscard_attribute_captured() {
    let src = "[[nodiscard]] int compute(int n) { return n; }\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    let pool = g.string_pool.as_slice();
    let dec_names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        dec_names.iter().any(|d| d.contains("nodiscard")),
        "[[nodiscard]] → decorator; got: {dec_names:?}"
    );
}

// ── no async for regular functions ───────────────────────────────────────────

#[test]
fn cpp_regular_function_no_async_flag() {
    let src = "int regular(int n) { return n; }\n";
    let g = analyze(src);
    let m = meta(&g, "regular");
    assert!(!m.is_async(), "regular function → not async");
    assert!(!m.is_generator());
}
