//! FunctionMeta extraction tests for Python.
//!
//! Each test builds a small fixture, runs the full analyzer pipeline, and
//! asserts that `ZeroCopyGraph::function_meta(node_idx)` returns the correct
//! flags, params, return_type, and decorators.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test_foo.py".as_ref(), src.as_bytes())
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
fn python_async_function_has_async_flag() {
    let src = "async def fetch_user(id: int) -> dict:\n    return {}\n";
    let g = analyze(src);
    let m = meta(&g, "fetch_user");
    assert!(m.is_async(), "expected is_async");
    assert!(!m.is_static());
    assert!(!m.is_generator());
    let pool = g.string_pool.as_slice();
    assert_eq!(m.params.len(), 2);
    assert_eq!(m.params[0].resolve(pool), "id");
    assert_eq!(m.params[1].resolve(pool), "int");
    assert_eq!(m.return_type.resolve(pool), "dict");
}

#[test]
fn python_sync_function_no_async_flag() {
    let src = "def greet(name: str) -> str:\n    return name\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert!(!m.is_async());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn python_staticmethod_has_static_flag() {
    let src = "class Foo:\n    @staticmethod\n    def make() -> 'Foo':\n        return Foo()\n";
    let g = analyze(src);
    let m = meta(&g, "make");
    assert!(m.is_static(), "expected is_static from @staticmethod");
}

#[test]
fn python_regular_method_no_static_flag() {
    let src = "class Foo:\n    def bar(self):\n        pass\n";
    let g = analyze(src);
    let m = meta(&g, "bar");
    assert!(!m.is_static());
}

// ── abstract ──────────────────────────────────────────────────────────────────

#[test]
fn python_abstractmethod_has_abstract_flag() {
    let src =
        "from abc import ABC, abstractmethod\nclass Base(ABC):\n    @abstractmethod\n    def compute(self) -> int:\n        pass\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(m.is_abstract(), "expected is_abstract from @abstractmethod");
}

// ── generator ─────────────────────────────────────────────────────────────────

#[test]
fn python_generator_has_generator_flag() {
    let src = "def count_up(n: int):\n    for i in range(n):\n        yield i\n";
    let g = analyze(src);
    let m = meta(&g, "count_up");
    assert!(m.is_generator(), "expected is_generator due to yield");
}

#[test]
fn python_yield_from_has_generator_flag() {
    let src = "def chain(a, b):\n    yield from a\n    yield from b\n";
    let g = analyze(src);
    let m = meta(&g, "chain");
    assert!(m.is_generator());
}

#[test]
fn python_non_generator_no_generator_flag() {
    let src = "def plain():\n    return 1\n";
    let g = analyze(src);
    let m = meta(&g, "plain");
    assert!(!m.is_generator());
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn python_public_function_visibility_zero() {
    let src = "def public_fn():\n    pass\n";
    let g = analyze(src);
    let m = meta(&g, "public_fn");
    assert_eq!(m.visibility(), 0, "public function → vis 0");
}

#[test]
fn python_private_underscore_function_visibility_two() {
    let src = "def _private():\n    pass\n";
    let g = analyze(src);
    let m = meta(&g, "_private");
    assert_eq!(m.visibility(), 2, "leading underscore → private (vis 2)");
}

#[test]
fn python_dunder_function_is_public() {
    let src = "class C:\n    def __init__(self):\n        pass\n";
    let g = analyze(src);
    let m = meta(&g, "__init__");
    assert_eq!(m.visibility(), 0, "__dunder__ → public (vis 0)");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn python_mixed_typed_untyped_params() {
    let src = "def process(x: int, y, z: str = 'a'):\n    pass\n";
    let g = analyze(src);
    let m = meta(&g, "process");
    let pool = g.string_pool.as_slice();
    // Expect: [x, int, y, "", z, str]
    assert!(m.params.len() >= 4, "at least x,int,y,\"\" pairs");
    assert_eq!(m.params[0].resolve(pool), "x");
    assert_eq!(m.params[1].resolve(pool), "int");
    assert_eq!(m.params[2].resolve(pool), "y");
    assert_eq!(m.params[3].resolve(pool), "");
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn python_return_type_captured() {
    let src = "def get_value() -> int:\n    return 42\n";
    let g = analyze(src);
    let m = meta(&g, "get_value");
    assert_eq!(m.return_type.resolve(g.string_pool.as_slice()), "int");
}

#[test]
fn python_no_return_type_is_empty() {
    let src = "def no_ret():\n    pass\n";
    let g = analyze(src);
    let m = meta(&g, "no_ret");
    assert_eq!(m.return_type.resolve(g.string_pool.as_slice()), "");
}

// ── decorators ────────────────────────────────────────────────────────────────

#[test]
fn python_multi_decorator_captured() {
    let src = "class C:\n    @staticmethod\n    @some_other\n    def multi():\n        pass\n";
    let g = analyze(src);
    let m = meta(&g, "multi");
    let pool = g.string_pool.as_slice();
    let dec_names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        dec_names.contains(&"staticmethod"),
        "staticmethod decorator expected, got: {dec_names:?}"
    );
    assert!(
        dec_names.contains(&"some_other"),
        "some_other decorator expected"
    );
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn python_test_name_prefix_is_test() {
    let src = "def test_user_login():\n    assert True\n";
    let g = analyze(src);
    let m = meta(&g, "test_user_login");
    assert!(m.is_test(), "test_ prefix → is_test");
}

#[test]
fn python_test_file_category_marks_is_test() {
    let src = "def login_flow():\n    pass\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "login_flow");
    assert!(m.is_test(), "test file category → is_test");
}

#[test]
fn python_pytest_decorator_marks_is_test() {
    let src = "import pytest\n@pytest.fixture\ndef my_fixture():\n    return {}\n";
    let g = analyze(src);
    let m = meta(&g, "my_fixture");
    assert!(m.is_test(), "pytest.fixture → is_test");
}

// ── sorted by node_idx invariant ─────────────────────────────────────────────

#[test]
fn python_function_metas_sorted_by_node_idx() {
    let src = "def a():\n    pass\ndef b():\n    pass\ndef c():\n    pass\n";
    let g = analyze(src);
    let indices: Vec<u32> = g.function_metas.iter().map(|m| m.node_idx).collect();
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    assert_eq!(indices, sorted, "function_metas must be sorted by node_idx");
    // Binary-search lookup must succeed for each.
    for m in &g.function_metas {
        assert!(g.function_meta(m.node_idx).is_some());
    }
}
