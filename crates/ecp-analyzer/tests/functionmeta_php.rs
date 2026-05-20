//! FunctionMeta extraction tests for PHP.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = PhpProvider::new().unwrap();
    let local = provider
        .parse_file("test.php".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = PhpProvider::new().unwrap();
    let local = provider
        .parse_file("UserTest.php".as_ref(), src.as_bytes())
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
fn php_public_method_visibility_zero() {
    let src = "<?php\nclass Foo {\n    public function bar(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "bar");
    assert_eq!(m.visibility(), 0, "public → vis 0");
}

#[test]
fn php_protected_method_visibility_one() {
    let src = "<?php\nclass Foo {\n    protected function baz(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "baz");
    assert_eq!(m.visibility(), 1, "protected → vis 1");
}

#[test]
fn php_private_method_visibility_two() {
    let src = "<?php\nclass Foo {\n    private function secret(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "secret");
    assert_eq!(m.visibility(), 2, "private → vis 2");
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn php_static_method_has_static_flag() {
    let src =
        "<?php\nclass Foo {\n    public static function make(): self { return new self(); }\n}\n";
    let g = analyze(src);
    let m = meta(&g, "make");
    assert!(m.is_static(), "static modifier → is_static");
}

#[test]
fn php_instance_method_no_static_flag() {
    let src = "<?php\nclass Foo {\n    public function run(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "run");
    assert!(!m.is_static());
}

// ── abstract ──────────────────────────────────────────────────────────────────

#[test]
fn php_abstract_method_has_abstract_flag() {
    let src = "<?php\nabstract class Base {\n    abstract public function compute(): int;\n}\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(m.is_abstract(), "abstract modifier → is_abstract");
}

#[test]
fn php_interface_method_is_abstract() {
    let src = "<?php\ninterface Runnable {\n    public function run(): void;\n}\n";
    let g = analyze(src);
    let m = meta(&g, "run");
    assert!(m.is_abstract(), "interface method → is_abstract");
}

// ── generator ─────────────────────────────────────────────────────────────────

#[test]
fn php_generator_function_has_generator_flag() {
    let src = "<?php\nfunction counter(int $max) {\n    for ($i = 0; $i < $max; $i++) {\n        yield $i;\n    }\n}\n";
    let g = analyze(src);
    let m = meta(&g, "counter");
    assert!(m.is_generator(), "yield → is_generator");
}

#[test]
fn php_regular_function_no_generator_flag() {
    let src = "<?php\nfunction add(int $a, int $b): int { return $a + $b; }\n";
    let g = analyze(src);
    let m = meta(&g, "add");
    assert!(!m.is_generator());
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn php_test_name_prefix_is_test() {
    let src = "<?php\nclass FooTest extends TestCase {\n    public function testUserLogin(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "testUserLogin");
    assert!(m.is_test(), "test* prefix → is_test");
}

#[test]
fn php_test_file_is_test() {
    let src =
        "<?php\nclass UserTest extends TestCase {\n    public function loginFlow(): void {}\n}\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "loginFlow");
    assert!(m.is_test(), "*Test.php file → is_test");
}

// ── params with type ──────────────────────────────────────────────────────────

#[test]
fn php_typed_params_captured() {
    let src = "<?php\nfunction process(int $count, string $name): void {}\n";
    let g = analyze(src);
    let m = meta(&g, "process");
    let pool = g.string_pool.as_slice();
    assert_eq!(m.params.len(), 4, "two params → 4 elements");
    assert_eq!(m.params[0].resolve(pool), "count");
    assert_eq!(m.params[1].resolve(pool), "int");
    assert_eq!(m.params[2].resolve(pool), "name");
    assert_eq!(m.params[3].resolve(pool), "string");
}

#[test]
fn php_union_type_param_captured() {
    let src = "<?php\nfunction accept(int|string $value): void {}\n";
    let g = analyze(src);
    let m = meta(&g, "accept");
    let pool = g.string_pool.as_slice();
    assert_eq!(m.params[0].resolve(pool), "value");
    let ty = m.params[1].resolve(pool);
    assert!(
        ty.contains("int") && ty.contains("string"),
        "union type captured; got: {ty:?}"
    );
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn php_return_type_captured() {
    let src = "<?php\nfunction getCount(): int { return 42; }\n";
    let g = analyze(src);
    let m = meta(&g, "getCount");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "int");
}

#[test]
fn php_no_return_type_is_empty() {
    let src = "<?php\nfunction doWork() {}\n";
    let g = analyze(src);
    let m = meta(&g, "doWork");
    assert_eq!(m.return_type.resolve(g.string_pool.as_slice()), "");
}

// ── decorators (PHP 8 attributes) ────────────────────────────────────────────

#[test]
fn php_attribute_captured_as_decorator() {
    let src = "<?php\nclass FooTest extends TestCase {\n    #[Test]\n    public function loginSucceeds(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "loginSucceeds");
    let pool = g.string_pool.as_slice();
    let dec_names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    assert!(
        dec_names.contains(&"Test"),
        "#[Test] attribute → decorator; got: {dec_names:?}"
    );
}

#[test]
fn php_attribute_marks_is_test() {
    let src = "<?php\nclass FooTest extends TestCase {\n    #[Test]\n    public function loginSucceeds(): void {}\n}\n";
    let g = analyze(src);
    let m = meta(&g, "loginSucceeds");
    assert!(m.is_test(), "#[Test] attribute → is_test");
}

// ── no async / no extern ──────────────────────────────────────────────────────

#[test]
fn php_no_async_flag() {
    let src = "<?php\nfunction fetch(): string { return ''; }\n";
    let g = analyze(src);
    let m = meta(&g, "fetch");
    assert!(!m.is_async(), "PHP has no async function keyword");
}

#[test]
fn php_no_extern_flag() {
    let src = "<?php\nfunction fetch(): string { return ''; }\n";
    let g = analyze(src);
    let m = meta(&g, "fetch");
    assert!(!m.is_extern(), "PHP has no extern concept");
}
