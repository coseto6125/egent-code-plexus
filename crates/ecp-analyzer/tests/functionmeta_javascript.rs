//! FunctionMeta extraction tests for JavaScript.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = JavaScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.js".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = JavaScriptProvider::new().unwrap();
    let local = provider
        .parse_file("foo.test.js".as_ref(), src.as_bytes())
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

// ── async ─────────────────────────────────────────────────────────────────────

#[test]
fn js_async_function_has_async_flag() {
    let src = "async function fetchData(url) { return fetch(url); }\n";
    let g = analyze(src);
    let m = meta(&g, "fetchData");
    assert!(m.is_async());
}

#[test]
fn js_sync_function_no_async_flag() {
    let src = "function greet(name) { return name; }\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert!(!m.is_async());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn js_static_method_has_static_flag() {
    let src = "class Foo {\n    static create() { return new Foo(); }\n}\n";
    let g = analyze(src);
    let m = meta(&g, "create");
    assert!(m.is_static());
}

#[test]
fn js_instance_method_no_static_flag() {
    let src = "class Foo {\n    run() {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "run");
    assert!(!m.is_static());
}

// ── generator ─────────────────────────────────────────────────────────────────

#[test]
fn js_generator_function_has_generator_flag() {
    let src = "function* sequence() { yield 1; yield 2; }\n";
    let g = analyze(src);
    let m = meta(&g, "sequence");
    assert!(m.is_generator());
}

#[test]
fn js_regular_function_no_generator_flag() {
    let src = "function plain() { return 1; }\n";
    let g = analyze(src);
    let m = meta(&g, "plain");
    assert!(!m.is_generator());
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn js_public_method_visibility_zero() {
    let src = "class C {\n    greet() {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert_eq!(m.visibility(), 0);
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn js_test_file_marks_is_test() {
    let src = "function login() {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "login");
    assert!(m.is_test(), "*.test.js file → is_test");
}

#[test]
fn js_it_function_name_is_test() {
    let src = "function it(name, fn) {}\n";
    let g = analyze(src);
    let m = meta(&g, "it");
    assert!(m.is_test(), "function named 'it' → is_test");
}

#[test]
fn js_describe_function_name_is_test() {
    let src = "function describe(name, fn) {}\n";
    let g = analyze(src);
    let m = meta(&g, "describe");
    assert!(m.is_test(), "function named 'describe' → is_test");
}

// ── params (no types in JS) ───────────────────────────────────────────────────

#[test]
fn js_params_names_captured_no_types() {
    let src = "function process(x, y, z) {}\n";
    let g = analyze(src);
    let m = meta(&g, "process");
    let pool = g.string_pool.as_slice();
    assert!(m.params.len() >= 2, "at least one name+empty_type pair");
    assert_eq!(m.params[0].resolve(pool), "x");
    assert_eq!(m.params[1].resolve(pool), ""); // no type in JS
}

// ── return_type (always empty in JS) ──────────────────────────────────────────

#[test]
fn js_no_return_type_annotation() {
    let src = "function getValue() { return 42; }\n";
    let g = analyze(src);
    let m = meta(&g, "getValue");
    assert_eq!(
        m.return_type.resolve(g.string_pool.as_slice()),
        "",
        "JS has no return type annotation"
    );
}

// ── sorted by node_idx invariant ─────────────────────────────────────────────

#[test]
fn js_function_metas_sorted_by_node_idx() {
    let src = "function a() {}\nfunction b() {}\nfunction c() {}\n";
    let g = analyze(src);
    let indices: Vec<u32> = g.function_metas.iter().map(|m| m.node_idx).collect();
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    assert_eq!(indices, sorted, "function_metas must be sorted by node_idx");
    for m in &g.function_metas {
        assert!(g.function_meta(m.node_idx).is_some());
    }
}

// ── multi-decorator (legacy/stage-3 decorators) ───────────────────────────────

#[test]
fn js_decorator_captured_if_present() {
    // Some JS environments support decorators; the RawNode.decorators already
    // captures them. This test just verifies no panic.
    let src = "class C {\n    run() {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "run");
    // decorators may be empty (no decorator in source) — that's correct.
    let _ = m.decorators.len();
}
