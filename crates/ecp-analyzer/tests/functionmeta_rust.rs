//! FunctionMeta extraction tests for Rust.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = RustProvider::new().unwrap();
    let local = provider
        .parse_file("test.rs".as_ref(), src.as_bytes())
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
fn rust_async_function_has_async_flag() {
    let src = "pub async fn fetch(id: u32) -> String { id.to_string() }\n";
    let g = analyze(src);
    let m = meta(&g, "fetch");
    assert!(m.is_async());
    assert!(!m.is_generator());
}

#[test]
fn rust_sync_function_no_async_flag() {
    let src = "fn greet(name: &str) -> String { name.to_string() }\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert!(!m.is_async());
}

// ── static (associated fn without self) ───────────────────────────────────────

#[test]
fn rust_associated_fn_no_self_is_static() {
    let src = "struct Foo;\nimpl Foo {\n    pub fn new() -> Foo { Foo }\n}\n";
    let g = analyze(src);
    let m = meta(&g, "new");
    assert!(m.is_static(), "associated fn with no self → static");
}

#[test]
fn rust_method_with_self_not_static() {
    let src = "struct Foo;\nimpl Foo {\n    pub fn bar(&self) -> u32 { 0 }\n}\n";
    let g = analyze(src);
    let m = meta(&g, "bar");
    assert!(!m.is_static(), "&self receiver → not static");
}

// ── abstract (trait body declaration without default) ─────────────────────────

#[test]
fn rust_trait_method_no_body_is_abstract() {
    let src = "trait Animal {\n    fn sound(&self) -> &str;\n}\n";
    let g = analyze(src);
    let m = meta(&g, "sound");
    assert!(
        m.is_abstract(),
        "fn signature without body in trait → abstract"
    );
}

#[test]
fn rust_trait_method_with_default_body_not_abstract() {
    let src = "trait Animal {\n    fn legs(&self) -> u8 { 4 }\n}\n";
    let g = analyze(src);
    let m = meta(&g, "legs");
    assert!(!m.is_abstract(), "fn with body in trait → not abstract");
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn rust_pub_function_visibility_zero() {
    let src = "pub fn open() {}\n";
    let g = analyze(src);
    let m = meta(&g, "open");
    assert_eq!(m.visibility(), 0, "pub → vis 0 (public)");
}

#[test]
fn rust_pub_crate_function_visibility_three() {
    let src = "pub(crate) fn internal() {}\n";
    let g = analyze(src);
    let m = meta(&g, "internal");
    assert_eq!(m.visibility(), 3, "pub(crate) → vis 3 (crate/internal)");
}

#[test]
fn rust_private_function_visibility_two() {
    let src = "fn private_fn() {}\n";
    let g = analyze(src);
    let m = meta(&g, "private_fn");
    assert_eq!(m.visibility(), 2, "no vis modifier → private (vis 2)");
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn rust_test_attribute_marks_is_test() {
    let src = "#[test]\nfn it_works() {\n    assert_eq!(2 + 2, 4);\n}\n";
    let g = analyze(src);
    let m = meta(&g, "it_works");
    assert!(m.is_test(), "#[test] → is_test");
}

#[test]
fn rust_tokio_test_attribute_marks_is_test() {
    let src = "#[tokio::test]\nasync fn async_test() {}\n";
    let g = analyze(src);
    let m = meta(&g, "async_test");
    assert!(m.is_test(), "#[tokio::test] → is_test");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn rust_params_name_and_type_captured() {
    let src = "fn add(x: i32, y: i32) -> i32 { x + y }\n";
    let g = analyze(src);
    let m = meta(&g, "add");
    let pool = g.string_pool.as_slice();
    assert!(m.params.len() >= 4, "expected at least [x,i32,y,i32]");
    assert_eq!(m.params[0].resolve(pool), "x");
    assert_eq!(m.params[1].resolve(pool), "i32");
    assert_eq!(m.params[2].resolve(pool), "y");
    assert_eq!(m.params[3].resolve(pool), "i32");
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn rust_return_type_captured() {
    let src = "fn compute() -> u64 { 0 }\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert!(!rt.is_empty(), "return type should not be empty");
    assert!(
        rt.contains("u64"),
        "return type should contain u64, got: {rt}"
    );
}

#[test]
fn rust_no_return_type_is_empty() {
    let src = "fn nothing() {}\n";
    let g = analyze(src);
    let m = meta(&g, "nothing");
    assert_eq!(m.return_type.resolve(g.string_pool.as_slice()), "");
}

// ── decorators (attributes) ───────────────────────────────────────────────────

#[test]
fn rust_attributes_captured_as_decorators() {
    let src = "#[inline]\n#[test]\nfn annotated() {}\n";
    let g = analyze(src);
    let m = meta(&g, "annotated");
    let pool = g.string_pool.as_slice();
    let dec_names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        dec_names.iter().any(|d| d.contains("inline")),
        "inline attribute expected, got: {dec_names:?}"
    );
    assert!(
        dec_names.iter().any(|d| d.contains("test")),
        "test attribute expected"
    );
}

// ── sorted by node_idx invariant ─────────────────────────────────────────────

#[test]
fn rust_function_metas_sorted_by_node_idx() {
    let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
    let g = analyze(src);
    let indices: Vec<u32> = g.function_metas.iter().map(|m| m.node_idx).collect();
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    assert_eq!(indices, sorted, "function_metas must be sorted by node_idx");
    for m in &g.function_metas {
        assert!(g.function_meta(m.node_idx).is_some());
    }
}
