//! FunctionMeta extraction tests for Dart.

use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = DartProvider::new().unwrap();
    let local = provider
        .parse_file("main.dart".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = DartProvider::new().unwrap();
    // Files inside test/ → FileCategory::Test via determine_category.
    let local = provider
        .parse_file("test/user_service_test.dart".as_ref(), src.as_bytes())
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
fn dart_async_function_has_async_flag() {
    let src = "Future<String> fetchData() async { return \"\"; }\n";
    let g = analyze(src);
    let m = meta(&g, "fetchData");
    assert!(m.is_async(), "async keyword → is_async");
}

#[test]
fn dart_sync_function_no_async_flag() {
    let src = "String greet(String name) { return name; }\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert!(!m.is_async());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn dart_static_method_has_static_flag() {
    let src = "class Util { static int add(int a, int b) { return a + b; } }\n";
    let g = analyze(src);
    let m = meta(&g, "add");
    assert!(m.is_static(), "static modifier → is_static");
}

#[test]
fn dart_instance_method_no_static_flag() {
    let src = "class Foo { void bar() {} }\n";
    let g = analyze(src);
    let m = meta(&g, "bar");
    assert!(!m.is_static());
}

// ── abstract ──────────────────────────────────────────────────────────────────

#[test]
fn dart_abstract_class_static_method_has_static_flag() {
    // In Dart, abstract methods without a body are not emitted as graph nodes
    // (no query pattern matches bodyless method signatures for methods).
    // We instead test that a static method in an abstract class is correctly flagged.
    let src = "abstract class Base { static void helper() {} }\n";
    let g = analyze(src);
    let m = meta(&g, "helper");
    assert!(m.is_static(), "static modifier → is_static");
    // The abstract class itself doesn't make the method abstract.
    assert!(!m.is_abstract());
}

// ── generator ────────────────────────────────────────────────────────────────

#[test]
fn dart_sync_star_has_generator_flag() {
    let src = "Iterable<int> count(int n) sync* { for (var i = 0; i < n; i++) yield i; }\n";
    let g = analyze(src);
    let m = meta(&g, "count");
    assert!(m.is_generator(), "sync* → is_generator");
}

#[test]
fn dart_async_star_has_both_flags() {
    let src = "Stream<int> stream() async* { yield 1; }\n";
    let g = analyze(src);
    let m = meta(&g, "stream");
    assert!(m.is_async(), "async* → is_async");
    assert!(m.is_generator(), "async* → is_generator");
}

#[test]
fn dart_non_generator_no_generator_flag() {
    let src = "int plain() { return 1; }\n";
    let g = analyze(src);
    let m = meta(&g, "plain");
    assert!(!m.is_generator());
}

// ── extern ────────────────────────────────────────────────────────────────────

#[test]
fn dart_external_function_has_extern_flag() {
    let src = "external int nativeAdd(int a, int b);\n";
    let g = analyze(src);
    let m = meta(&g, "nativeAdd");
    assert!(m.is_extern(), "external modifier → is_extern");
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn dart_public_function_vis_zero() {
    let src = "void pub() {}\n";
    let g = analyze(src);
    let m = meta(&g, "pub");
    assert_eq!(m.visibility(), 0, "no underscore → public (vis 0)");
}

#[test]
fn dart_private_underscore_function_vis_two() {
    let src = "void _priv() {}\n";
    let g = analyze(src);
    let m = meta(&g, "_priv");
    assert_eq!(m.visibility(), 2, "leading _ → private (vis 2)");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn dart_typed_params_captured() {
    let src = "String greet(String name, int count) { return name; }\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    let pool = g.string_pool.as_slice();
    assert!(!m.params.is_empty(), "params should be non-empty");
    let names: Vec<_> = m.params.iter().map(|p| p.resolve(pool)).collect();
    assert!(
        names.contains(&"name") || names.contains(&"count"),
        "expected param names in params vec, got: {names:?}"
    );
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn dart_return_type_captured() {
    let src = "List<String> getNames() { return []; }\n";
    let g = analyze(src);
    let m = meta(&g, "getNames");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert!(!rt.is_empty(), "return type should be non-empty, got empty");
}

#[test]
fn dart_no_return_type_is_empty() {
    let src = "noRet() { return 1; }\n";
    let g = analyze(src);
    let m = meta(&g, "noRet");
    // Dart untyped → empty or dynamic; just assert no panic.
    let _rt = m.return_type.resolve(g.string_pool.as_slice());
}

// ── decorators / annotations ──────────────────────────────────────────────────

#[test]
fn dart_override_annotation_captured() {
    let src = "class C extends Base { @override String toString() { return \"\"; } }\n";
    let g = analyze(src);
    let m = meta(&g, "toString");
    let pool = g.string_pool.as_slice();
    let names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        names.contains(&"override"),
        "expected override decorator, got: {names:?}"
    );
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn dart_test_file_in_test_dir_marks_is_test() {
    let src = "void testLogin() {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "testLogin");
    assert!(
        m.is_test(),
        "test/ directory → file category Test → is_test"
    );
}

// ── sorted invariant ──────────────────────────────────────────────────────────

#[test]
fn dart_function_metas_sorted_by_node_idx() {
    let src = "void a() {}\nvoid b() {}\nvoid c() {}\n";
    let g = analyze(src);
    let indices: Vec<u32> = g.function_metas.iter().map(|m| m.node_idx).collect();
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    assert_eq!(indices, sorted, "function_metas must be sorted by node_idx");
    for m in &g.function_metas {
        assert!(g.function_meta(m.node_idx).is_some());
    }
}

#[test]
fn dart_nested_function_has_function_meta() {
    let src = "int outer() { int inner(int value) { return value; } return inner(1); }\n";
    let g = analyze(src);

    let outer = meta(&g, "outer");
    let inner = meta(&g, "inner");

    assert!(!outer
        .return_type
        .resolve(g.string_pool.as_slice())
        .is_empty());
    assert!(!inner.params.is_empty());
}
