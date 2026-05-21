//! FunctionMeta extraction tests for Ruby.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = RubyProvider::new().unwrap();
    let local = provider
        .parse_file("test.rb".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_spec_file(src: &str) -> ZeroCopyGraph {
    let provider = RubyProvider::new().unwrap();
    let local = provider
        .parse_file("user_spec.rb".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = RubyProvider::new().unwrap();
    let local = provider
        .parse_file("user_test.rb".as_ref(), src.as_bytes())
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

// ── static (class method) ─────────────────────────────────────────────────────

#[test]
fn ruby_singleton_method_has_static_flag() {
    let src = "class Foo\n  def self.create\n    new\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "create");
    assert!(m.is_static(), "def self.foo → is_static");
}

#[test]
fn ruby_instance_method_no_static_flag() {
    let src = "class Foo\n  def run\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "run");
    assert!(!m.is_static(), "def foo → not is_static");
}

// ── abstract (heuristic) ──────────────────────────────────────────────────────

#[test]
fn ruby_raise_not_implemented_error_is_abstract() {
    let src = "class Base\n  def compute\n    raise NotImplementedError\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(
        m.is_abstract(),
        "raise NotImplementedError → is_abstract (heuristic)"
    );
}

// ── generator ─────────────────────────────────────────────────────────────────

#[test]
fn ruby_method_with_yield_has_generator_flag() {
    let src = "class Iterator\n  def each\n    yield 1\n    yield 2\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "each");
    assert!(m.is_generator(), "yield → is_generator");
}

#[test]
fn ruby_method_without_yield_no_generator_flag() {
    let src = "class Foo\n  def plain\n    42\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "plain");
    assert!(!m.is_generator());
}

// ── visibility ────────────────────────────────────────────────────────────────

#[test]
fn ruby_default_method_is_public() {
    let src = "class Foo\n  def public_method\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "public_method");
    assert_eq!(m.visibility(), 0, "default → public (vis 0)");
}

#[test]
fn ruby_private_section_method_is_private() {
    let src = "class Foo\n  private\n  def secret\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "secret");
    assert_eq!(m.visibility(), 2, "after private → vis 2");
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn ruby_minitest_name_prefix_is_test() {
    let src = "class UserTest < Minitest::Test\n  def test_login\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "test_login");
    assert!(m.is_test(), "test_ prefix → is_test");
}

#[test]
fn ruby_spec_file_is_test() {
    let src = "class Foo\n  def validate\n  end\nend\n";
    let g = analyze_spec_file(src);
    let m = meta(&g, "validate");
    assert!(m.is_test(), "_spec.rb file → is_test");
}

#[test]
fn ruby_test_file_is_test() {
    let src = "class Bar\n  def helper\n  end\nend\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "helper");
    assert!(m.is_test(), "_test.rb file → is_test");
}

// ── params (no type annotation in stock Ruby) ─────────────────────────────────

#[test]
fn ruby_params_captured_no_type() {
    let src = "class Foo\n  def greet(name, greeting)\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "greet");
    let pool = g.string_pool.as_slice();
    assert_eq!(m.params.len(), 4, "two params → 4 elements");
    assert_eq!(m.params[0].resolve(pool), "name");
    assert_eq!(m.params[1].resolve(pool), "", "type is empty in Ruby");
    assert_eq!(m.params[2].resolve(pool), "greeting");
    assert_eq!(m.params[3].resolve(pool), "");
}

// ── return_type always empty ──────────────────────────────────────────────────

#[test]
fn ruby_return_type_always_empty() {
    let src = "class Foo\n  def count\n    42\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "count");
    assert_eq!(
        m.return_type.resolve(g.string_pool.as_slice()),
        "",
        "Ruby has no return type annotations"
    );
}

// ── decorators always empty ───────────────────────────────────────────────────

#[test]
fn ruby_decorators_always_empty() {
    let src = "class Foo\n  def plain\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "plain");
    assert!(
        m.decorators.is_empty(),
        "Ruby has no decorator syntax in stock language"
    );
}

// ── no async / no extern ──────────────────────────────────────────────────────

#[test]
fn ruby_no_async_flag() {
    let src = "class Foo\n  def fetch\n  end\nend\n";
    let g = analyze(src);
    let m = meta(&g, "fetch");
    assert!(!m.is_async());
    assert!(!m.is_extern());
}
