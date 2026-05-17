//! Regression tests for three previously-missing Dart symbol-emission paths:
//!
//! 1. Constructor — all four grammar forms: default, named, factory, const.
//! 2. Enum — both plain and Dart 2.17+ enhanced enum with a method.
//! 3. Annotation — custom `@Foo()` annotation at declaration site.
//!
//! These tests were written BEFORE the fixes and were expected to fail
//! against the original queries.scm / spec.rs.

use graph_nexus_analyzer::dart::parser::DartProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = DartProvider::new().expect("provider");
    p.parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse")
}

fn count(graph: &LocalGraph, kind: NodeKind) -> usize {
    graph.nodes.iter().filter(|n| n.kind == kind).count()
}

fn has(graph: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    graph.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

// ── Constructor ─────────────────────────────────────────────────────────────

/// Default constructor: `Foo()` → method_declaration > method_signature >
/// constructor_signature.
#[test]
fn constructor_default_emits() {
    let g = parse("class Foo { Foo() {} }");
    assert!(
        has(&g, "Foo", NodeKind::Constructor),
        "default constructor must emit; nodes: {:#?}",
        g.nodes
    );
}

/// Named constructor: `Foo.named()` — grammar emits two identifier children
/// for the name field (`Foo` and `named`). Both are captured; at minimum the
/// constructor-specific suffix "named" must appear.
#[test]
fn constructor_named_emits() {
    let g = parse("class Foo { Foo.named() {} }");
    assert!(
        has(&g, "named", NodeKind::Constructor),
        "named constructor suffix 'named' must emit; nodes: {:#?}",
        g.nodes
    );
}

/// Factory constructor: `factory Foo.fromJson(...)` — grammar uses
/// factory_constructor_signature inside method_signature, which the original
/// query did NOT match.
#[test]
fn constructor_factory_emits() {
    let g = parse("class Foo { factory Foo.fromJson(Map<String,dynamic> m) => Foo._(); Foo._(); }");
    assert!(
        has(&g, "fromJson", NodeKind::Constructor),
        "factory constructor 'fromJson' must emit; nodes: {:#?}",
        g.nodes
    );
}

/// Const constructor: `const Foo.constant()` — grammar uses
/// constant_constructor_signature inside declaration (NOT method_declaration),
/// which the original query did NOT match.
#[test]
fn constructor_const_emits() {
    let g = parse("class Foo { const Foo.constant(); }");
    assert!(
        has(&g, "constant", NodeKind::Constructor) || has(&g, "Foo", NodeKind::Constructor),
        "const constructor must emit at least one Constructor node; nodes: {:#?}",
        g.nodes
    );
}

/// Combined: class with all four constructor flavors must emit ≥ 3 Constructor
/// nodes (the count may be higher because named/factory constructors emit both
/// the class-name identifier and the suffix identifier).
#[test]
fn constructor_all_flavors_emit_at_least_three() {
    let src = r#"
class Foo {
  Foo();
  Foo.named();
  factory Foo.fromJson(Map<String,dynamic> m) => Foo.named();
  const Foo.constant();
}
"#;
    let g = parse(src);
    let n = count(&g, NodeKind::Constructor);
    assert!(
        n >= 3,
        "expected ≥ 3 Constructor nodes for four flavors, got {n}; nodes: {:#?}",
        g.nodes
    );
}

// ── Enum ─────────────────────────────────────────────────────────────────────

/// Plain enum: `enum Color { red, green, blue }`.
#[test]
fn enum_plain_emits() {
    let g = parse("enum Color { red, green, blue }");
    assert!(
        has(&g, "Color", NodeKind::Enum),
        "plain enum 'Color' must emit NodeKind::Enum; nodes: {:#?}",
        g.nodes
    );
    // Must NOT be classified as Interface (old mis-mapping).
    assert!(
        !has(&g, "Color", NodeKind::Interface),
        "enum 'Color' must NOT be NodeKind::Interface after fix; nodes: {:#?}",
        g.nodes
    );
}

/// Enhanced enum (Dart 2.17+) with implements clause and a method.
#[test]
fn enum_enhanced_emits() {
    let src = r#"
enum Planet implements Comparable<Planet> {
  mercury(3.303e+23, 2.4397e6),
  venus(4.869e+24, 6.0518e6);

  const Planet(this.mass, this.radius);
  final double mass;
  final double radius;
  double get surfaceGravity => 6.674e-11 * mass / (radius * radius);
}
"#;
    let g = parse(src);
    assert!(
        has(&g, "Planet", NodeKind::Enum),
        "enhanced enum 'Planet' must emit NodeKind::Enum; nodes: {:#?}",
        g.nodes
    );
}

/// Multiple enums in one file — both must emit.
#[test]
fn enum_multiple_emit() {
    let src = "enum Status { active, inactive } enum Role { admin, user }";
    let g = parse(src);
    assert!(
        has(&g, "Status", NodeKind::Enum) && has(&g, "Role", NodeKind::Enum),
        "both enums must emit; nodes: {:#?}",
        g.nodes
    );
    assert_eq!(
        count(&g, NodeKind::Enum),
        2,
        "exactly 2 Enum nodes expected; nodes: {:#?}",
        g.nodes
    );
}

// ── Annotation ───────────────────────────────────────────────────────────────

/// Built-in `@override` annotation on a method.
#[test]
fn annotation_override_emits() {
    let src = r#"
class Child extends Base {
  @override
  void doWork() {}
}
"#;
    let g = parse(src);
    assert!(
        has(&g, "override", NodeKind::Annotation),
        "@override must emit NodeKind::Annotation; nodes: {:#?}",
        g.nodes
    );
}

/// Custom annotation `@Immutable()` on a class.
#[test]
fn annotation_custom_class_emits() {
    let src = r#"
@Immutable()
class Config {
  final String host;
  const Config(this.host);
}
"#;
    let g = parse(src);
    assert!(
        has(&g, "Immutable", NodeKind::Annotation),
        "@Immutable must emit NodeKind::Annotation; nodes: {:#?}",
        g.nodes
    );
}

/// Multiple annotations on one declaration.
#[test]
fn annotation_multiple_on_decl_emit() {
    let src = r#"
class Foo {
  @override
  @deprecated
  void oldMethod() {}
}
"#;
    let g = parse(src);
    assert!(
        has(&g, "override", NodeKind::Annotation),
        "@override must emit; nodes: {:#?}",
        g.nodes
    );
    assert!(
        has(&g, "deprecated", NodeKind::Annotation),
        "@deprecated must emit; nodes: {:#?}",
        g.nodes
    );
    assert!(
        count(&g, NodeKind::Annotation) >= 2,
        "at least 2 Annotation nodes expected; nodes: {:#?}",
        g.nodes
    );
}
