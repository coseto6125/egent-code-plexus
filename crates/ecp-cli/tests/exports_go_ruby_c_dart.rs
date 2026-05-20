//! Integration tests: export detection for Go, Ruby, C, and Dart.
//!
//! Each language uses a different convention:
//!   - Go:   identifier starts with uppercase → exported
//!   - Ruby: `private`/`protected` markers change method visibility
//!   - C:    `static` storage-class specifier → not exported
//!   - Dart: identifier starts with `_` → not exported (library-private)

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;

fn parse_go(src: &str) -> Vec<RawNode> {
    GoProvider::new()
        .unwrap()
        .parse_file("test.go".as_ref(), src.as_bytes())
        .unwrap()
        .nodes
}

fn parse_ruby(src: &str) -> Vec<RawNode> {
    RubyProvider::new()
        .unwrap()
        .parse_file("test.rb".as_ref(), src.as_bytes())
        .unwrap()
        .nodes
}

fn parse_c(src: &str) -> Vec<RawNode> {
    CProvider::new()
        .unwrap()
        .parse_file("test.c".as_ref(), src.as_bytes())
        .unwrap()
        .nodes
}

fn parse_dart(src: &str) -> Vec<RawNode> {
    DartProvider::new()
        .unwrap()
        .parse_file("test.dart".as_ref(), src.as_bytes())
        .unwrap()
        .nodes
}

fn is_exported(nodes: &[RawNode], name: &str) -> Option<bool> {
    nodes.iter().find(|n| n.name == name).map(|n| n.is_exported)
}

// --- Go ---

#[test]
fn go_uppercase_function_is_exported() {
    let nodes = parse_go("func PublicFunc() {}\n");
    assert_eq!(
        is_exported(&nodes, "PublicFunc"),
        Some(true),
        "Go uppercase identifier must be exported"
    );
}

#[test]
fn go_lowercase_function_is_not_exported() {
    let nodes = parse_go("func privateFunc() {}\n");
    assert_eq!(
        is_exported(&nodes, "privateFunc"),
        Some(false),
        "Go lowercase identifier must not be exported"
    );
}

#[test]
fn go_uppercase_type_is_exported() {
    let src = "type Server struct { Port int }\n";
    let nodes = parse_go(src);
    assert_eq!(
        is_exported(&nodes, "Server"),
        Some(true),
        "Go uppercase struct must be exported"
    );
}

#[test]
fn go_lowercase_type_is_not_exported() {
    let src = "type config struct { debug bool }\n";
    let nodes = parse_go(src);
    assert_eq!(
        is_exported(&nodes, "config"),
        Some(false),
        "Go lowercase struct must not be exported"
    );
}

// --- Ruby ---

#[test]
fn ruby_method_before_private_is_exported() {
    let src = "class Foo\n  def public_method\n  end\n  private\n  def secret\n  end\nend\n";
    let nodes = parse_ruby(src);
    assert_eq!(
        is_exported(&nodes, "public_method"),
        Some(true),
        "Ruby method before `private` must be exported"
    );
}

#[test]
fn ruby_method_after_private_is_not_exported() {
    let src = "class Foo\n  def public_method\n  end\n  private\n  def secret\n  end\nend\n";
    let nodes = parse_ruby(src);
    assert_eq!(
        is_exported(&nodes, "secret"),
        Some(false),
        "Ruby method after `private` must not be exported"
    );
}

#[test]
fn ruby_method_after_protected_is_not_exported() {
    let src = "class Bar\n  protected\n  def guarded\n  end\nend\n";
    let nodes = parse_ruby(src);
    assert_eq!(
        is_exported(&nodes, "guarded"),
        Some(false),
        "Ruby method after `protected` must not be exported"
    );
}

#[test]
fn ruby_public_marker_restores_exported() {
    let src = "class Baz\n  private\n  def secret\n  end\n  public\n  def visible\n  end\nend\n";
    let nodes = parse_ruby(src);
    assert_eq!(
        is_exported(&nodes, "secret"),
        Some(false),
        "Ruby method after `private` must not be exported"
    );
    assert_eq!(
        is_exported(&nodes, "visible"),
        Some(true),
        "Ruby method after restored `public` must be exported"
    );
}

// --- C ---

#[test]
fn c_regular_function_is_exported() {
    let nodes = parse_c("int public_fn(int x) { return x; }\n");
    assert_eq!(
        is_exported(&nodes, "public_fn"),
        Some(true),
        "C function without static must be exported"
    );
}

#[test]
fn c_static_function_is_not_exported() {
    let nodes = parse_c("static int private_fn(int x) { return x; }\n");
    assert_eq!(
        is_exported(&nodes, "private_fn"),
        Some(false),
        "C static function must not be exported"
    );
}

#[test]
fn c_static_inline_function_is_not_exported() {
    let nodes = parse_c("static inline void helper(void) {}\n");
    assert_eq!(
        is_exported(&nodes, "helper"),
        Some(false),
        "C static inline function must not be exported"
    );
}

// --- Dart ---

#[test]
fn dart_public_function_is_exported() {
    let nodes = parse_dart("void publicFn() {}\n");
    assert_eq!(
        is_exported(&nodes, "publicFn"),
        Some(true),
        "Dart identifier without leading _ must be exported"
    );
}

#[test]
fn dart_private_function_is_not_exported() {
    let nodes = parse_dart("void _privateFn() {}\n");
    assert_eq!(
        is_exported(&nodes, "_privateFn"),
        Some(false),
        "Dart identifier with leading _ must not be exported"
    );
}

#[test]
fn dart_public_class_is_exported() {
    let nodes = parse_dart("class MyService {}\n");
    assert_eq!(
        is_exported(&nodes, "MyService"),
        Some(true),
        "Dart public class must be exported"
    );
}

#[test]
fn dart_private_class_is_not_exported() {
    let nodes = parse_dart("class _InternalImpl {}\n");
    assert_eq!(
        is_exported(&nodes, "_InternalImpl"),
        Some(false),
        "Dart private class must not be exported"
    );
}
