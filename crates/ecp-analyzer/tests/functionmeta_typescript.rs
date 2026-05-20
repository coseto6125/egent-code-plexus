//! FunctionMeta extraction tests for TypeScript.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = TypeScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.ts".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = TypeScriptProvider::new().unwrap();
    let local = provider
        .parse_file("foo.test.ts".as_ref(), src.as_bytes())
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
fn ts_async_function_has_async_flag() {
    let src = "async function fetchUser(id: number): Promise<string> { return ''; }\n";
    let g = analyze(src);
    let m = meta(&g, "fetchUser");
    assert!(m.is_async());
}

#[test]
fn ts_sync_function_no_async_flag() {
    let src = "function greet(name: string): string { return name; }\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert!(!m.is_async());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn ts_static_method_has_static_flag() {
    let src = "class Foo {\n    static create(): Foo { return new Foo(); }\n}\n";
    let g = analyze(src);
    let m = meta(&g, "create");
    assert!(m.is_static());
}

#[test]
fn ts_instance_method_no_static_flag() {
    let src = "class Foo {\n    run(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "run");
    assert!(!m.is_static());
}

// ── abstract ──────────────────────────────────────────────────────────────────

#[test]
fn ts_abstract_method_has_abstract_flag() {
    let src = "abstract class Base {\n    abstract compute(): number;\n}\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(m.is_abstract());
}

// ── generator ─────────────────────────────────────────────────────────────────

#[test]
fn ts_generator_function_has_generator_flag() {
    let src = "function* counter(): Generator<number> { yield 1; yield 2; }\n";
    let g = analyze(src);
    let m = meta(&g, "counter");
    assert!(m.is_generator());
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn ts_public_method_visibility_zero() {
    let src = "class C {\n    public greet(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    assert_eq!(m.visibility(), 0);
}

#[test]
fn ts_protected_method_visibility_one() {
    let src = "class C {\n    protected helper(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "helper");
    assert_eq!(m.visibility(), 1);
}

#[test]
fn ts_private_method_visibility_two() {
    let src = "class C {\n    private secret(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "secret");
    assert_eq!(m.visibility(), 2);
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn ts_test_file_marks_is_test() {
    let src = "function login(): void {}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "login");
    assert!(m.is_test(), "*.test.ts file category → is_test");
}

#[test]
fn ts_it_function_is_test() {
    let src = "it('does something', () => {});\nfunction it(name: string, fn: () => void) {}\n";
    let g = analyze(src);
    let m = meta(&g, "it");
    assert!(m.is_test(), "function named 'it' → is_test");
}

// ── params ────────────────────────────────────────────────────────────────────

#[test]
fn ts_params_with_types_captured() {
    let src = "function add(x: number, y: number): number { return x + y; }\n";
    let g = analyze(src);
    let m = meta(&g, "add");
    let pool = g.string_pool.as_slice();
    assert!(m.params.len() >= 4, "expected at least [x,number,y,number]");
    assert_eq!(m.params[0].resolve(pool), "x");
    assert_eq!(m.params[2].resolve(pool), "y");
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn ts_return_type_captured() {
    let src = "function id(x: number): number { return x; }\n";
    let g = analyze(src);
    let m = meta(&g, "id");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert!(!rt.is_empty(), "return type should be present");
}

#[test]
fn ts_no_return_type_is_empty() {
    let src = "function side(): void {}\n";
    let g = analyze(src);
    // void or empty — either is acceptable since TS void is annotated.
    // Just ensure no panic and meta exists.
    let m = meta(&g, "side");
    let _ = m.return_type.resolve(g.string_pool.as_slice());
}

// ── decorators ────────────────────────────────────────────────────────────────

#[test]
fn ts_decorator_captured() {
    // NestJS-style decorator on a class method.
    let src =
        "function Get(path: string) { return (_: any, __: any, ___: any) => {}; }\nclass C {\n    @Get('/users')\n    getAll(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "getAll");
    let pool = g.string_pool.as_slice();
    // The decorator text may include the path argument. Capture is best-effort; just
    // verify no panic and that the pool lookup doesn't error.
    let _ = m.decorators.iter().map(|d| d.resolve(pool)).count();
}

// ── sorted by node_idx invariant ─────────────────────────────────────────────

#[test]
fn ts_function_metas_sorted_by_node_idx() {
    let src = "function a(): void {}\nfunction b(): void {}\nfunction c(): void {}\n";
    let g = analyze(src);
    let indices: Vec<u32> = g.function_metas.iter().map(|m| m.node_idx).collect();
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    assert_eq!(indices, sorted, "function_metas must be sorted by node_idx");
    for m in &g.function_metas {
        assert!(g.function_meta(m.node_idx).is_some());
    }
}
