//! FunctionMeta extraction tests for Go.

use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = GoProvider::new().unwrap();
    let local = provider
        .parse_file("test.go".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = GoProvider::new().unwrap();
    let local = provider
        .parse_file("foo_test.go".as_ref(), src.as_bytes())
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
fn go_exported_function_is_public() {
    let src = "package main\nfunc Exported() {}\n";
    let g = analyze(src);
    let m = meta(&g, "Exported");
    assert_eq!(m.visibility(), 0, "uppercase first letter → public (vis 0)");
    assert!(!m.is_async());
    assert!(!m.is_static());
    assert!(!m.is_abstract());
    assert!(!m.is_generator());
}

#[test]
fn go_unexported_function_is_private() {
    let src = "package main\nfunc unexported() {}\n";
    let g = analyze(src);
    let m = meta(&g, "unexported");
    assert_eq!(
        m.visibility(),
        2,
        "lowercase first letter → package-private (vis 2)"
    );
}

// ── is_static / is_async / is_abstract / is_generator ────────────────────────

#[test]
fn go_no_static_flag() {
    // Go has no static methods — is_static must always be false.
    let src = "package main\nfunc GlobalFn() {}\n";
    let g = analyze(src);
    let m = meta(&g, "GlobalFn");
    assert!(!m.is_static(), "Go has no static methods");
}

#[test]
fn go_no_async_flag() {
    // Go uses goroutines via `go` statement — no function-level async.
    let src = "package main\nfunc LaunchSomething() { go func() {}() }\n";
    let g = analyze(src);
    let m = meta(&g, "LaunchSomething");
    assert!(!m.is_async(), "Go has no async functions");
}

#[test]
fn go_no_generator_flag() {
    // Go uses channels instead of yield-based generators.
    let src = "package main\nfunc Produce(ch chan int) { ch <- 1 }\n";
    let g = analyze(src);
    let m = meta(&g, "Produce");
    assert!(!m.is_generator());
}

// ── extern (no body / cgo) ────────────────────────────────────────────────────

#[test]
fn go_function_with_body_not_extern() {
    let src = "package main\nfunc WithBody() int { return 1 }\n";
    let g = analyze(src);
    let m = meta(&g, "WithBody");
    assert!(!m.is_extern(), "function with body is not extern");
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn go_test_function_in_test_file() {
    let src = "package main\nfunc TestLoginFlow(t *testing.T) {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "TestLoginFlow");
    assert!(m.is_test(), "Test* in _test.go → is_test");
}

#[test]
fn go_benchmark_in_test_file() {
    let src = "package main\nfunc BenchmarkParse(b *testing.B) {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "BenchmarkParse");
    assert!(m.is_test(), "Benchmark* in _test.go → is_test");
}

#[test]
fn go_example_in_test_file() {
    let src = "package main\nfunc ExampleFoo() {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "ExampleFoo");
    assert!(m.is_test(), "Example* in _test.go → is_test");
}

#[test]
fn go_fuzz_in_test_file() {
    let src = "package main\nfunc FuzzParser(f *testing.F) {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "FuzzParser");
    assert!(m.is_test(), "Fuzz* in _test.go → is_test");
}

#[test]
fn go_test_name_but_not_test_file_is_not_test() {
    // Test* name but in a non-_test.go file → NOT is_test.
    let src = "package main\nfunc TestHelper() bool { return true }\n";
    let g = analyze(src);
    let m = meta(&g, "TestHelper");
    assert!(!m.is_test(), "Test* in non-test file → not is_test");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn go_params_with_types_captured() {
    let src = "package main\nfunc Add(a int, b int) int { return a + b }\n";
    let g = analyze(src);
    let m = meta(&g, "Add");
    let pool = g.string_pool.as_slice();
    assert_eq!(
        m.params.len(),
        4,
        "two params → 4 elements (name, type × 2)"
    );
    assert_eq!(m.params[0].resolve(pool), "a");
    assert_eq!(m.params[1].resolve(pool), "int");
    assert_eq!(m.params[2].resolve(pool), "b");
    assert_eq!(m.params[3].resolve(pool), "int");
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn go_single_return_type_captured() {
    let src = "package main\nfunc GetName() string { return \"\" }\n";
    let g = analyze(src);
    let m = meta(&g, "GetName");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "string");
}

#[test]
fn go_multi_return_type_captured_as_literal() {
    // Multi-return: `(int, error)` — captured as the full parameter_list text.
    let src = "package main\nfunc Divide(a, b int) (int, error) { return a / b, nil }\n";
    let g = analyze(src);
    let m = meta(&g, "Divide");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert!(
        rt.contains("int") && rt.contains("error"),
        "multi-return captured as literal; got: {rt:?}"
    );
}

#[test]
fn go_no_return_type_is_empty() {
    let src = "package main\nfunc DoNothing() {}\n";
    let g = analyze(src);
    let m = meta(&g, "DoNothing");
    assert_eq!(m.return_type.resolve(g.string_pool.as_slice()), "");
}

// ── decorators (go: directives) ───────────────────────────────────────────────

#[test]
fn go_noinline_directive_captured_as_decorator() {
    let src = "package main\n//go:noinline\nfunc HotPath() {}\n";
    let g = analyze(src);
    let m = meta(&g, "HotPath");
    let pool = g.string_pool.as_slice();
    let dec_names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        dec_names.iter().any(|d| d.contains("noinline")),
        "//go:noinline → decorator; got: {dec_names:?}"
    );
}
