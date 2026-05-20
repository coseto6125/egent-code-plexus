//! FunctionMeta extraction tests for Java.

use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = JavaProvider::new().unwrap();
    let local = provider
        .parse_file("Main.java".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = JavaProvider::new().unwrap();
    let local = provider
        .parse_file("UserServiceTest.java".as_ref(), src.as_bytes())
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

// ── async (Java has none) ─────────────────────────────────────────────────────

#[test]
fn java_no_async_flag_even_for_completable_future() {
    let src = "class Svc { public CompletableFuture<String> fetch() { return null; } }";
    let g = analyze(src);
    let m = meta(&g, "fetch");
    // Java has no language-level async; CompletableFuture is library-level.
    assert!(!m.is_async(), "Java should never set is_async");
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn java_static_method_has_static_flag() {
    let src = "class Util { public static int add(int a, int b) { return a + b; } }";
    let g = analyze(src);
    let m = meta(&g, "add");
    assert!(m.is_static(), "static modifier → is_static");
}

#[test]
fn java_instance_method_no_static_flag() {
    let src = "class Foo { public void bar() {} }";
    let g = analyze(src);
    let m = meta(&g, "bar");
    assert!(!m.is_static());
}

// ── abstract ──────────────────────────────────────────────────────────────────

#[test]
fn java_abstract_method_has_abstract_flag() {
    let src = "abstract class Base { public abstract void compute(); }";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(m.is_abstract(), "abstract modifier → is_abstract");
}

#[test]
fn java_interface_method_is_abstract() {
    let src = "interface Repo { List<User> findAll(); }";
    let g = analyze(src);
    let m = meta(&g, "findAll");
    assert!(
        m.is_abstract(),
        "interface method without body → is_abstract"
    );
}

// ── generator (Java has none) ─────────────────────────────────────────────────

#[test]
fn java_never_has_generator_flag() {
    let src = "class G { public void produce() { /* no yield */ } }";
    let g = analyze(src);
    let m = meta(&g, "produce");
    assert!(!m.is_generator(), "Java has no yield → never is_generator");
}

// ── extern / native ───────────────────────────────────────────────────────────

#[test]
fn java_native_method_has_extern_flag() {
    let src = "class Jni { public native int nativeAdd(int a, int b); }";
    let g = analyze(src);
    let m = meta(&g, "nativeAdd");
    assert!(m.is_extern(), "native modifier → is_extern");
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn java_public_method_vis_zero() {
    let src = "class C { public void pub() {} }";
    let g = analyze(src);
    let m = meta(&g, "pub");
    assert_eq!(m.visibility(), 0, "public → vis 0");
}

#[test]
fn java_protected_method_vis_one() {
    let src = "class C { protected void prot() {} }";
    let g = analyze(src);
    let m = meta(&g, "prot");
    assert_eq!(m.visibility(), 1, "protected → vis 1");
}

#[test]
fn java_private_method_vis_two() {
    let src = "class C { private void priv() {} }";
    let g = analyze(src);
    let m = meta(&g, "priv");
    assert_eq!(m.visibility(), 2, "private → vis 2");
}

#[test]
fn java_package_private_method_vis_four() {
    let src = "class C { void pkg() {} }";
    let g = analyze(src);
    let m = meta(&g, "pkg");
    assert_eq!(m.visibility(), 4, "package-private (no modifier) → vis 4");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn java_typed_params_captured() {
    let src = "class C { public String greet(String name, int count) { return name; } }";
    let g = analyze(src);
    let m = meta(&g, "greet");
    let pool = g.string_pool.as_slice();
    assert_eq!(
        m.params.len(),
        4,
        "two params → 4 entries [name,type,name,type]"
    );
    assert_eq!(m.params[0].resolve(pool), "name");
    assert_eq!(m.params[1].resolve(pool), "String");
    assert_eq!(m.params[2].resolve(pool), "count");
    assert_eq!(m.params[3].resolve(pool), "int");
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn java_return_type_captured() {
    let src = "class C { public List<User> getUsers() { return null; } }";
    let g = analyze(src);
    let m = meta(&g, "getUsers");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert!(!rt.is_empty(), "return_type should be populated");
    assert!(rt.contains("List"), "expected List<User>, got: {rt}");
}

#[test]
fn java_void_return_type_captured() {
    let src = "class C { public void doSomething() {} }";
    let g = analyze(src);
    let m = meta(&g, "doSomething");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "void");
}

// ── decorators / annotations ──────────────────────────────────────────────────

#[test]
fn java_override_annotation_captured() {
    let src = "class C extends Base { @Override public String toString() { return \"\"; } }";
    let g = analyze(src);
    let m = meta(&g, "toString");
    let pool = g.string_pool.as_slice();
    let names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        names.contains(&"Override"),
        "expected Override annotation, got: {names:?}"
    );
}

#[test]
fn java_deprecated_annotation_captured() {
    let src = "class C { @Deprecated public void old() {} }";
    let g = analyze(src);
    let m = meta(&g, "old");
    let pool = g.string_pool.as_slice();
    let names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        names.contains(&"Deprecated"),
        "expected Deprecated, got: {names:?}"
    );
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn java_test_annotation_marks_is_test() {
    let src = "class C { @Test public void shouldReturnUser() {} }";
    let g = analyze(src);
    let m = meta(&g, "shouldReturnUser");
    assert!(m.is_test(), "@Test → is_test");
}

#[test]
fn java_test_file_category_marks_is_test() {
    let src = "class UserServiceTest { public void testLogin() {} }";
    let g = analyze_test_file(src);
    let m = meta(&g, "testLogin");
    assert!(m.is_test(), "file category Test → is_test");
}

#[test]
fn java_parameterized_test_annotation_marks_is_test() {
    let src = "class C { @ParameterizedTest void testWithParam(int x) {} }";
    let g = analyze(src);
    let m = meta(&g, "testWithParam");
    assert!(m.is_test(), "@ParameterizedTest → is_test");
}

// ── sorted invariant ──────────────────────────────────────────────────────────

#[test]
fn java_function_metas_sorted_by_node_idx() {
    let src = "class C { public void a() {} public void b() {} public void c() {} }";
    let g = analyze(src);
    let indices: Vec<u32> = g.function_metas.iter().map(|m| m.node_idx).collect();
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    assert_eq!(indices, sorted, "function_metas must be sorted by node_idx");
    for m in &g.function_metas {
        assert!(g.function_meta(m.node_idx).is_some());
    }
}
