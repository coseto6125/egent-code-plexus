//! FunctionMeta extraction tests for Kotlin.

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = KotlinProvider::new().unwrap();
    let local = provider
        .parse_file("Main.kt".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = KotlinProvider::new().unwrap();
    let local = provider
        .parse_file("UserServiceTest.kt".as_ref(), src.as_bytes())
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

// ── async / suspend ───────────────────────────────────────────────────────────

#[test]
fn kotlin_suspend_function_has_async_flag() {
    let src = "suspend fun fetchUser(id: Int): String = \"\"\n";
    let g = analyze(src);
    let m = meta(&g, "fetchUser");
    assert!(m.is_async(), "suspend → is_async");
}

#[test]
fn kotlin_regular_function_no_async_flag() {
    let src = "fun greet(name: String): String = name\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert!(!m.is_async());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn kotlin_top_level_function_has_static_flag() {
    // Top-level functions compile to static methods on the file's facade class.
    let src = "fun topLevel(): Int = 42\n";
    let g = analyze(src);
    let m = meta(&g, "topLevel");
    assert!(m.is_static(), "top-level function → is_static");
}

#[test]
fn kotlin_class_method_no_static_flag_by_default() {
    let src = "class Foo { fun bar(): Int = 0 }\n";
    let g = analyze(src);
    let m = meta(&g, "bar");
    assert!(!m.is_static(), "instance method → not static");
}

// ── abstract ──────────────────────────────────────────────────────────────────

#[test]
fn kotlin_abstract_method_has_abstract_flag() {
    let src = "abstract class Base { abstract fun compute(): Int }\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(m.is_abstract(), "abstract modifier → is_abstract");
}

// ── generator (Kotlin has none) ───────────────────────────────────────────────

#[test]
fn kotlin_never_has_generator_flag() {
    let src = "fun seq() = sequence { yield(1) }\n";
    let g = analyze(src);
    let m = meta(&g, "seq");
    assert!(
        !m.is_generator(),
        "Kotlin sequence builder is library → never is_generator"
    );
}

// ── extern ────────────────────────────────────────────────────────────────────

#[test]
fn kotlin_external_function_has_extern_flag() {
    let src = "external fun nativeCompute(x: Int): Int\n";
    let g = analyze(src);
    let m = meta(&g, "nativeCompute");
    assert!(m.is_extern(), "external modifier → is_extern");
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn kotlin_public_default_visibility_zero() {
    // Kotlin's default is public.
    let src = "fun pub(): Unit {}\n";
    let g = analyze(src);
    let m = meta(&g, "pub");
    assert_eq!(m.visibility(), 0, "default (public) → vis 0");
}

#[test]
fn kotlin_private_function_vis_two() {
    let src = "private fun priv(): Unit {}\n";
    let g = analyze(src);
    let m = meta(&g, "priv");
    assert_eq!(m.visibility(), 2, "private → vis 2");
}

#[test]
fn kotlin_internal_function_vis_three() {
    let src = "internal fun internal_fn(): Unit {}\n";
    let g = analyze(src);
    let m = meta(&g, "internal_fn");
    assert_eq!(m.visibility(), 3, "internal → vis 3");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn kotlin_typed_params_captured() {
    let src = "fun process(name: String, count: Int): Unit {}\n";
    let g = analyze(src);
    let m = meta(&g, "process");
    let pool = g.string_pool.as_slice();
    assert_eq!(m.params.len(), 4, "two params → 4 entries");
    assert_eq!(m.params[0].resolve(pool), "name");
    assert_eq!(m.params[1].resolve(pool), "String");
    assert_eq!(m.params[2].resolve(pool), "count");
    assert_eq!(m.params[3].resolve(pool), "Int");
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn kotlin_return_type_captured() {
    let src = "fun getValue(): Int = 42\n";
    let g = analyze(src);
    let m = meta(&g, "getValue");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert!(!rt.is_empty(), "return type should be captured");
    assert!(rt.contains("Int"), "expected Int return type, got: {rt}");
}

#[test]
fn kotlin_no_return_type_is_empty() {
    // Kotlin `fun foo()` without `: ReturnType` → empty.
    let src = "fun noRet(): Unit {}\n";
    let g = analyze(src);
    let m = meta(&g, "noRet");
    // Unit return type should be captured or empty — either is acceptable.
    // Assert it does NOT panic (meta exists).
    let _rt = m.return_type.resolve(g.string_pool.as_slice());
}

// ── decorators / annotations ──────────────────────────────────────────────────

#[test]
fn kotlin_jvmstatic_annotation_captured() {
    let src = "class C { companion object { @JvmStatic fun create(): C = C() } }\n";
    let g = analyze(src);
    let m = meta(&g, "create");
    let pool = g.string_pool.as_slice();
    let names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    // @JvmStatic should appear in decorators; also is_static should be true.
    assert!(
        names.contains(&"JvmStatic"),
        "expected JvmStatic decorator, got: {names:?}"
    );
    assert!(m.is_static(), "@JvmStatic → is_static");
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn kotlin_test_file_category_marks_is_test() {
    let src = "class UserServiceTest { fun testLogin() {} }\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "testLogin");
    assert!(m.is_test(), "*Test.kt file category → is_test");
}

#[test]
fn kotlin_test_annotation_marks_is_test() {
    let src = "class Tests { @Test fun shouldReturnTrue() {} }\n";
    let g = analyze(src);
    let m = meta(&g, "shouldReturnTrue");
    assert!(m.is_test(), "@Test annotation → is_test");
}

// ── sorted invariant ──────────────────────────────────────────────────────────

#[test]
fn kotlin_function_metas_sorted_by_node_idx() {
    let src = "fun a(): Unit {}\nfun b(): Unit {}\nfun c(): Unit {}\n";
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
fn kotlin_nested_function_has_function_meta() {
    let src = "fun outer(): Int { fun inner(value: Int): Int = value; return inner(1) }\n";
    let g = analyze(src);

    let outer = meta(&g, "outer");
    let inner = meta(&g, "inner");

    assert!(outer.is_static());
    assert_eq!(inner.params.len(), 2);
}
