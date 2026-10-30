//! `DartProvider` defects found in the driver-move review
//! (FU-2026-10-30-732420b06a19). Each test asserts the correct records, so
//! it fails until the fix lands.

mod driver_parity_support;

use driver_parity_support::{names_kinds, Span4};
use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::overrides::is_marked;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &[u8]) -> LocalGraph {
    DartProvider::new()
        .expect("DartProvider::new")
        .parse_file(Path::new("a.dart"), src)
        .expect("parse_file")
}

fn node<'g>(g: &'g LocalGraph, name: &str, kind: NodeKind) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} `{name}` in {:?}", names_kinds(g)))
}

/// `(name, kind, span, owner_class, is_exported)` of each node, in emitted
/// order.
fn rows(g: &LocalGraph) -> Vec<(&str, NodeKind, Span4, Option<&str>, bool)> {
    g.nodes
        .iter()
        .map(|n| {
            (
                n.name.as_str(),
                n.kind,
                n.span,
                n.owner_class.as_deref(),
                n.is_exported,
            )
        })
        .collect()
}

const ANNOTATED: &[u8] = b"@immutable\nclass Dog {\n  @override\n  String toString() => 'dog';\n  @Deprecated('use bark2')\n  void bark() {}\n  @meta.visibleForTesting\n  int count = 0;\n  @override\n  @Deprecated('x')\n  void both() {}\n  void plain() {}\n}\n@pragma('vm:entry-point')\nvoid main() {}\n";

/// Each annotation reaches the decorators of the declaration it marks, as
/// its source text (the Kotlin and C# convention), in source order. A
/// field's annotation sits beside its declaration and still counts.
#[test]
fn test_dart_annotations_reach_declaration_decorators() {
    let g = parse(ANNOTATED);
    let decorators = |name: &str, kind: NodeKind| node(&g, name, kind).decorators.clone();
    assert_eq!(decorators("Dog", NodeKind::Class), ["@immutable"]);
    assert_eq!(decorators("toString", NodeKind::Method), ["@override"]);
    assert_eq!(
        decorators("bark", NodeKind::Method),
        ["@Deprecated('use bark2')"]
    );
    assert_eq!(
        decorators("count", NodeKind::Property),
        ["@meta.visibleForTesting"]
    );
    assert_eq!(
        decorators("both", NodeKind::Method),
        ["@override", "@Deprecated('x')"]
    );
    assert_eq!(
        decorators("main", NodeKind::Function),
        ["@pragma('vm:entry-point')"]
    );
    assert!(decorators("plain", NodeKind::Method).is_empty());
    assert!(g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Annotation)
        .all(|n| n.decorators.is_empty()));
}

/// `@override` now marks the method for the `Overrides` pass; an
/// unrelated annotation does not.
#[test]
fn test_dart_override_annotation_marks_method() {
    let g = parse(ANNOTATED);
    assert!(is_marked(
        &node(&g, "toString", NodeKind::Method).decorators
    ));
    assert!(is_marked(&node(&g, "both", NodeKind::Method).decorators));
    assert!(!is_marked(&node(&g, "bark", NodeKind::Method).decorators));
}

/// Guard: the decorator fix adds no node and moves none; the Annotation
/// nodes stay.
#[test]
fn test_dart_annotated_file_node_rows_unchanged() {
    let g = parse(ANNOTATED);
    let dog = Some("Dog");
    assert_eq!(
        rows(&g),
        [
            ("immutable", NodeKind::Annotation, (0, 0, 0, 10), dog, true),
            ("Dog", NodeKind::Class, (0, 0, 12, 1), None, true),
            ("override", NodeKind::Annotation, (2, 2, 2, 11), dog, true),
            ("toString", NodeKind::Method, (2, 2, 3, 29), dog, true),
            ("Deprecated", NodeKind::Annotation, (4, 2, 4, 26), dog, true),
            ("bark", NodeKind::Method, (4, 2, 5, 16), dog, true),
            (
                "visibleForTesting",
                NodeKind::Annotation,
                (6, 2, 6, 25),
                dog,
                true
            ),
            ("count", NodeKind::Property, (7, 2, 7, 15), dog, true),
            ("override", NodeKind::Annotation, (8, 2, 8, 11), dog, true),
            ("Deprecated", NodeKind::Annotation, (9, 2, 9, 18), dog, true),
            ("both", NodeKind::Method, (8, 2, 10, 16), dog, true),
            ("plain", NodeKind::Method, (11, 2, 11, 17), dog, true),
            ("pragma", NodeKind::Annotation, (13, 0, 13, 25), None, true),
            ("main", NodeKind::Function, (13, 0, 14, 14), None, true),
        ]
    );
}

/// A mixin's `on` types are its superclass constraints, so they are
/// heritage, in source order before `implements`. A generic constraint
/// keeps the type name only, as a class's `Comparable<Dog>` does.
#[test]
fn test_dart_mixin_on_clause_types_are_heritage() {
    let g = parse(
        b"mixin Walker on Animal {}\nmixin Swimmer on Animal, Fish implements Mover {}\nmixin Plain {}\nmixin Hydrated<S> on BlocBase<S> {}\nclass Dog extends Animal with Walker implements Comparable<Dog> {}\n",
    );
    let heritage = |name: &str, kind: NodeKind| node(&g, name, kind).heritage.clone();
    assert_eq!(heritage("Walker", NodeKind::Trait), ["Animal"]);
    assert_eq!(
        heritage("Swimmer", NodeKind::Trait),
        ["Animal", "Fish", "Mover"]
    );
    assert_eq!(heritage("Hydrated", NodeKind::Trait), ["BlocBase"]);
    assert!(heritage("Plain", NodeKind::Trait).is_empty());
    assert_eq!(
        heritage("Dog", NodeKind::Class),
        ["Animal", "Walker", "Comparable"]
    );
}

/// A named constructor `Dog.named(...)` is one Constructor node, named by
/// its own identifier, in all four grammar forms; an unnamed one keeps the
/// class name, and a private `Cat._()` follows the `_` privacy rule.
#[test]
fn test_dart_named_constructor_emits_one_node_per_declaration() {
    let g = parse(
        b"class Dog {\n  Dog();\n  Dog.named(this.n);\n  factory Dog.fromJson(Map m) => Dog();\n  const Dog.constant();\n  Dog.body() {}\n  final int n = 0;\n}\nclass Cat {\n  factory Cat() => Cat._();\n  Cat._();\n}\n",
    );
    let (dog, cat) = (Some("Dog"), Some("Cat"));
    assert_eq!(
        rows(&g),
        [
            ("Dog", NodeKind::Class, (0, 0, 7, 1), None, true),
            ("Dog", NodeKind::Constructor, (1, 2, 1, 7), dog, true),
            ("named", NodeKind::Constructor, (2, 2, 2, 19), dog, true),
            ("fromJson", NodeKind::Constructor, (3, 2, 3, 39), dog, true),
            ("constant", NodeKind::Constructor, (4, 2, 4, 22), dog, true),
            ("body", NodeKind::Constructor, (5, 2, 5, 15), dog, true),
            ("n", NodeKind::Property, (6, 2, 6, 17), dog, true),
            ("Cat", NodeKind::Class, (8, 0, 11, 1), None, true),
            ("Cat", NodeKind::Constructor, (9, 2, 9, 27), cat, true),
            ("_", NodeKind::Constructor, (10, 2, 10, 9), cat, false),
        ]
    );
}
