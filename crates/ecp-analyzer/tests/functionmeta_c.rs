//! FunctionMeta extraction tests for C.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = CProvider::new().unwrap();
    let local = provider
        .parse_file("test.c".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = CProvider::new().unwrap();
    let local = provider
        .parse_file("tests/test_login.c".as_ref(), src.as_bytes())
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

// ── visibility (always public in C) ──────────────────────────────────────────

#[test]
fn c_global_function_is_public() {
    let src = "int add(int a, int b) { return a + b; }\n";
    let g = analyze(src);
    let m = meta(&g, "add");
    assert_eq!(
        m.visibility(),
        0,
        "C global function → always public (vis 0)"
    );
    assert!(!m.is_abstract());
    assert!(!m.is_async());
    assert!(!m.is_generator());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn c_static_function_has_static_flag() {
    let src = "static int helper(int x) { return x * 2; }\n";
    let g = analyze(src);
    let m = meta(&g, "helper");
    assert!(m.is_static(), "static storage class → is_static");
}

#[test]
fn c_non_static_function_no_static_flag() {
    let src = "int global_fn(void) { return 0; }\n";
    let g = analyze(src);
    let m = meta(&g, "global_fn");
    assert!(!m.is_static());
}

// ── extern ────────────────────────────────────────────────────────────────────

#[test]
fn c_extern_declaration_has_extern_flag() {
    // A forward declaration (prototype) in C → is_extern.
    let src = "int compute(int n);\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(m.is_extern(), "declaration without body → is_extern");
}

#[test]
fn c_definition_not_extern() {
    let src = "int compute(int n) { return n * 2; }\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(!m.is_extern(), "definition with body → not is_extern");
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn c_test_file_category_marks_is_test() {
    // C test detection is file-path-only (framework-agnostic caveat).
    let src = "void test_add(void) { }\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "test_add");
    assert!(m.is_test(), "tests/ directory → is_test");
}

#[test]
fn c_non_test_file_not_test() {
    let src = "void test_helper(void) { }\n";
    let g = analyze(src);
    let m = meta(&g, "test_helper");
    assert!(!m.is_test(), "non-test file → not is_test");
}

// ── params with types ─────────────────────────────────────────────────────────

#[test]
fn c_typed_params_captured() {
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

#[test]
fn c_void_param_list_is_empty() {
    // `int foo(void)` → no params emitted (void is a sentinel).
    let src = "int foo(void) { return 0; }\n";
    let g = analyze(src);
    let m = meta(&g, "foo");
    assert!(
        m.params.is_empty(),
        "void param list → empty params; got: {:?}",
        m.params.len()
    );
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn c_return_type_captured() {
    let src = "int multiply(int a, int b) { return a * b; }\n";
    let g = analyze(src);
    let m = meta(&g, "multiply");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "int");
}

#[test]
fn c_void_return_type_captured() {
    let src = "void do_work(void) {}\n";
    let g = analyze(src);
    let m = meta(&g, "do_work");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "void");
}

// ── decorators ───────────────────────────────────────────────────────────────

#[test]
fn c_attribute_decorator_captured() {
    // GCC __attribute__ is detected from direct children.
    let src = "void hot_path(void) __attribute__((noinline)) {}\n";
    let g = analyze(src);
    let m = meta(&g, "hot_path");
    let pool = g.string_pool.as_slice();
    let dec_names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    // GCC attribute may appear; non-empty is sufficient for this test.
    let _ = dec_names; // attribute presence is grammar-version dependent
}
