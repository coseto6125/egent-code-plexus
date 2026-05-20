//! FunctionMeta extraction tests for Swift.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = SwiftProvider::new().unwrap();
    let local = provider
        .parse_file("Main.swift".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = SwiftProvider::new().unwrap();
    let local = provider
        .parse_file("UserServiceTests.swift".as_ref(), src.as_bytes())
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
fn swift_async_function_has_async_flag() {
    let src = "func fetchData() async -> String { return \"\" }\n";
    let g = analyze(src);
    let m = meta(&g, "fetchData");
    assert!(m.is_async(), "async keyword → is_async");
}

#[test]
fn swift_sync_function_no_async_flag() {
    let src = "func greet(name: String) -> String { return name }\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert!(!m.is_async());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn swift_static_function_has_static_flag() {
    let src = "class Foo { static func make() -> Foo { return Foo() } }\n";
    let g = analyze(src);
    let m = meta(&g, "make");
    assert!(m.is_static(), "static keyword → is_static");
}

#[test]
fn swift_instance_method_no_static_flag() {
    let src = "class Foo { func bar() {} }\n";
    let g = analyze(src);
    let m = meta(&g, "bar");
    assert!(!m.is_static());
}

// ── abstract (protocol requirement) ──────────────────────────────────────────

#[test]
fn swift_protocol_requirement_is_abstract() {
    let src = "protocol Repo { func findAll() -> [String] }\n";
    let g = analyze(src);
    let m = meta(&g, "findAll");
    assert!(m.is_abstract(), "protocol requirement → is_abstract");
}

// ── generator (Swift has none) ────────────────────────────────────────────────

#[test]
fn swift_never_has_generator_flag() {
    let src = "func seq() -> AnySequence<Int> { return AnySequence([]) }\n";
    let g = analyze(src);
    let m = meta(&g, "seq");
    assert!(!m.is_generator(), "Swift has no language-level generators");
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn swift_public_function_vis_zero() {
    let src = "public func pub() {}\n";
    let g = analyze(src);
    let m = meta(&g, "pub");
    assert_eq!(m.visibility(), 0, "public → vis 0");
}

#[test]
fn swift_private_function_vis_two() {
    let src = "private func priv() {}\n";
    let g = analyze(src);
    let m = meta(&g, "priv");
    assert_eq!(m.visibility(), 2, "private → vis 2");
}

#[test]
fn swift_internal_default_vis_three() {
    // No access modifier → Swift default is `internal` → vis 3.
    let src = "func internalFn() {}\n";
    let g = analyze(src);
    let m = meta(&g, "internalFn");
    assert_eq!(m.visibility(), 3, "internal (default) → vis 3");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn swift_params_captured_internal_name() {
    // `func greet(to name: String)` — external label `to` is dropped; `name` is captured.
    let src = "func greet(name: String, count: Int) -> String { return name }\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    let pool = g.string_pool.as_slice();
    // At least one param pair with name and type.
    assert!(!m.params.is_empty(), "params should be non-empty");
    let names: Vec<_> = m.params.iter().map(|p| p.resolve(pool)).collect();
    assert!(
        names.contains(&"name") || names.contains(&"count"),
        "expected param names, got: {names:?}"
    );
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn swift_return_type_captured() {
    let src = "func getValue() -> Int { return 42 }\n";
    let g = analyze(src);
    let m = meta(&g, "getValue");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert!(!rt.is_empty(), "return type should be non-empty");
    assert!(rt.contains("Int"), "expected Int, got: {rt}");
}

#[test]
fn swift_no_return_type_is_empty() {
    let src = "func noRet() {}\n";
    let g = analyze(src);
    let m = meta(&g, "noRet");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "", "absent return annotation → empty");
}

// ── decorators / attributes ───────────────────────────────────────────────────

#[test]
fn swift_main_actor_attribute_captured() {
    let src = "@MainActor\nfunc updateUI() {}\n";
    let g = analyze(src);
    let m = meta(&g, "updateUI");
    let pool = g.string_pool.as_slice();
    let names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        names.contains(&"MainActor"),
        "expected MainActor decorator, got: {names:?}"
    );
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn swift_test_function_name_prefix_is_test() {
    let src = "func testLogin() {}\n";
    let g = analyze(src);
    let m = meta(&g, "testLogin");
    assert!(m.is_test(), "name starts with test → is_test");
}

#[test]
fn swift_test_file_category_marks_is_test() {
    let src = "func login() {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "login");
    assert!(m.is_test(), "*Tests.swift file category → is_test");
}

// ── sorted invariant ──────────────────────────────────────────────────────────

#[test]
fn swift_function_metas_sorted_by_node_idx() {
    let src = "func a() {}\nfunc b() {}\nfunc c() {}\n";
    let g = analyze(src);
    let indices: Vec<u32> = g.function_metas.iter().map(|m| m.node_idx).collect();
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    assert_eq!(indices, sorted, "function_metas must be sorted by node_idx");
    for m in &g.function_metas {
        assert!(g.function_meta(m.node_idx).is_some());
    }
}
