//! FunctionMeta extraction tests for C#.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = CSharpProvider::new().unwrap();
    let local = provider
        .parse_file("Main.cs".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = CSharpProvider::new().unwrap();
    let local = provider
        .parse_file("UserServiceTest.cs".as_ref(), src.as_bytes())
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
fn csharp_async_method_has_async_flag() {
    let src = "class C { public async Task<string> FetchAsync() { return \"\"; } }";
    let g = analyze(src);
    let m = meta(&g, "FetchAsync");
    assert!(m.is_async(), "async modifier → is_async");
}

#[test]
fn csharp_sync_method_no_async_flag() {
    let src = "class C { public string Greet() { return \"\"; } }";
    let g = analyze(src);
    let m = meta(&g, "Greet");
    assert!(!m.is_async());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn csharp_static_method_has_static_flag() {
    let src = "class C { public static int Add(int a, int b) { return a + b; } }";
    let g = analyze(src);
    let m = meta(&g, "Add");
    assert!(m.is_static(), "static modifier → is_static");
}

#[test]
fn csharp_instance_method_no_static_flag() {
    let src = "class C { public void Do() {} }";
    let g = analyze(src);
    let m = meta(&g, "Do");
    assert!(!m.is_static());
}

// ── abstract ──────────────────────────────────────────────────────────────────

#[test]
fn csharp_abstract_method_has_abstract_flag() {
    let src = "abstract class Base { public abstract int Compute(); }";
    let g = analyze(src);
    let m = meta(&g, "Compute");
    assert!(m.is_abstract(), "abstract modifier → is_abstract");
}

// ── generator ────────────────────────────────────────────────────────────────

#[test]
fn csharp_yield_return_has_generator_flag() {
    let src = "class C { public IEnumerable<int> Count(int n) { for (int i = 0; i < n; i++) yield return i; } }";
    let g = analyze(src);
    let m = meta(&g, "Count");
    assert!(m.is_generator(), "yield return → is_generator");
}

#[test]
fn csharp_yield_break_has_generator_flag() {
    let src = "class C { public IEnumerable<int> Empty() { yield break; } }";
    let g = analyze(src);
    let m = meta(&g, "Empty");
    assert!(m.is_generator(), "yield break → is_generator");
}

#[test]
fn csharp_non_generator_no_generator_flag() {
    let src = "class C { public int Plain() { return 1; } }";
    let g = analyze(src);
    let m = meta(&g, "Plain");
    assert!(!m.is_generator());
}

// ── extern ────────────────────────────────────────────────────────────────────

#[test]
fn csharp_extern_method_has_extern_flag() {
    let src =
        "[DllImport(\"native.dll\")] class C { public static extern int NativeAdd(int a, int b); }";
    let g = analyze(src);
    let m = meta(&g, "NativeAdd");
    assert!(m.is_extern(), "extern modifier → is_extern");
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn csharp_public_method_vis_zero() {
    let src = "class C { public void Pub() {} }";
    let g = analyze(src);
    let m = meta(&g, "Pub");
    assert_eq!(m.visibility(), 0, "public → vis 0");
}

#[test]
fn csharp_protected_method_vis_one() {
    let src = "class C { protected void Prot() {} }";
    let g = analyze(src);
    let m = meta(&g, "Prot");
    assert_eq!(m.visibility(), 1, "protected → vis 1");
}

#[test]
fn csharp_private_method_vis_two() {
    let src = "class C { private void Priv() {} }";
    let g = analyze(src);
    let m = meta(&g, "Priv");
    assert_eq!(m.visibility(), 2, "private → vis 2");
}

#[test]
fn csharp_internal_method_vis_three() {
    let src = "class C { internal void Intern() {} }";
    let g = analyze(src);
    let m = meta(&g, "Intern");
    assert_eq!(m.visibility(), 3, "internal → vis 3");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn csharp_typed_params_captured() {
    let src = "class C { public string Greet(string name, int count) { return name; } }";
    let g = analyze(src);
    let m = meta(&g, "Greet");
    let pool = g.string_pool.as_slice();
    assert_eq!(m.params.len(), 4, "two params → 4 entries");
    assert_eq!(m.params[0].resolve(pool), "name");
    assert_eq!(m.params[1].resolve(pool), "string");
    assert_eq!(m.params[2].resolve(pool), "count");
    assert_eq!(m.params[3].resolve(pool), "int");
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn csharp_return_type_captured() {
    let src = "class C { public List<string> GetNames() { return null; } }";
    let g = analyze(src);
    let m = meta(&g, "GetNames");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert!(!rt.is_empty(), "return type should be non-empty");
    assert!(rt.contains("List"), "expected List<string>, got: {rt}");
}

#[test]
fn csharp_void_return_type_is_empty() {
    let src = "class C { public void DoWork() {} }";
    let g = analyze(src);
    let m = meta(&g, "DoWork");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "", "void → empty return_type");
}

// ── decorators / attributes ───────────────────────────────────────────────────

#[test]
fn csharp_attribute_captured() {
    let src = "class C { [Obsolete] public void OldMethod() {} }";
    let g = analyze(src);
    let m = meta(&g, "OldMethod");
    let pool = g.string_pool.as_slice();
    let names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        names.contains(&"Obsolete"),
        "expected Obsolete attribute, got: {names:?}"
    );
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn csharp_test_attribute_marks_is_test() {
    let src = "class Tests { [Test] public void ShouldPass() {} }";
    let g = analyze(src);
    let m = meta(&g, "ShouldPass");
    assert!(m.is_test(), "[Test] attribute → is_test");
}

#[test]
fn csharp_fact_attribute_marks_is_test() {
    let src = "class Tests { [Fact] public void ShouldBeTrue() {} }";
    let g = analyze(src);
    let m = meta(&g, "ShouldBeTrue");
    assert!(m.is_test(), "[Fact] attribute → is_test");
}

#[test]
fn csharp_test_file_category_marks_is_test() {
    let src = "class UserServiceTest { public void TestLogin() {} }";
    let g = analyze_test_file(src);
    let m = meta(&g, "TestLogin");
    assert!(m.is_test(), "file category Test → is_test");
}

// ── sorted invariant ──────────────────────────────────────────────────────────

#[test]
fn csharp_function_metas_sorted_by_node_idx() {
    let src = "class C { public void A() {} public void B() {} public void C_() {} }";
    let g = analyze(src);
    let indices: Vec<u32> = g.function_metas.iter().map(|m| m.node_idx).collect();
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    assert_eq!(indices, sorted, "function_metas must be sorted by node_idx");
    for m in &g.function_metas {
        assert!(g.function_meta(m.node_idx).is_some());
    }
}
