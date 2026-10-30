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
/// nodes stay. The class annotation `@immutable` has no owner: it marks
/// `Dog` and sits outside every type (a separate fix, tested below).
#[test]
fn test_dart_annotated_file_node_rows_unchanged() {
    let g = parse(ANNOTATED);
    let dog = Some("Dog");
    assert_eq!(
        rows(&g),
        [
            // Contract: an annotation on a top-level type is not owned by it.
            ("immutable", NodeKind::Annotation, (0, 0, 0, 10), None, true),
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

/// An annotation written before a class, mixin, enum or extension marks that
/// declaration, so it takes the declaration's owner (none at top level), not
/// the declaration itself. A member annotation stays owned by the type.
#[test]
fn test_dart_type_annotation_not_owned_by_annotated_type() {
    let g = parse(
        b"@a\nclass K {\n  @b\n  void m() {}\n}\n@c\nmixin M {}\n@d\nenum E { x }\n@e\nextension X on int {}\n",
    );
    let owners: Vec<(&str, Option<&str>)> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Annotation)
        .map(|n| (n.name.as_str(), n.owner_class.as_deref()))
        .collect();
    assert_eq!(
        owners,
        [
            ("a", None),
            ("b", Some("K")),
            ("c", None),
            ("d", None),
            ("e", None),
        ]
    );
    assert_eq!(node(&g, "K", NodeKind::Class).decorators, ["@a"]);
    assert_eq!(node(&g, "M", NodeKind::Trait).decorators, ["@c"]);
    assert_eq!(node(&g, "E", NodeKind::Enum).decorators, ["@d"]);
    assert_eq!(node(&g, "X", NodeKind::Trait).decorators, ["@e"]);
}

/// Annotations on a top-level variable, a getter, a body-less named
/// constructor and a `const` constructor reach their decorators too; a
/// second annotation on a field does not become the first one's decorator.
#[test]
fn test_dart_annotations_on_variables_getters_and_constructors() {
    let g = parse(
        b"@b\nfinal int top = 1;\nclass K {\n  @d\n  K.named();\n  @e\n  const K.c();\n  @f\n  int get g => 1;\n  @x\n  @y\n  int z = 0;\n}\n",
    );
    let decorators = |name: &str, kind: NodeKind| node(&g, name, kind).decorators.clone();
    assert_eq!(decorators("top", NodeKind::Variable), ["@b"]);
    assert_eq!(decorators("named", NodeKind::Constructor), ["@d"]);
    assert_eq!(decorators("c", NodeKind::Constructor), ["@e"]);
    assert_eq!(decorators("g", NodeKind::Property), ["@f"]);
    assert_eq!(decorators("z", NodeKind::Property), ["@x", "@y"]);
    assert!(g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Annotation)
        .all(|n| n.decorators.is_empty()));
}

const ABSTRACT_MEMBERS: &[u8] = b"abstract class Cipher {\n  @override\n  int one();\n  @override\n  @useResult\n  int two(int x);\n  int none();\n  @override\n  void body() {}\n  @a\n  int get g;\n  @b\n  set s(int v);\n  @o\n  bool operator ==(Object other);\n}\n@pragma('x')\nvoid top() {}\n";

/// FU-B1: an annotation on a body-less member reaches its decorators, in
/// source order; zero annotations stay empty; a member with a body is
/// unchanged from round 1.
#[test]
fn test_dart_abstract_member_annotations_reach_decorators() {
    let g = parse(ABSTRACT_MEMBERS);
    let decorators = |name: &str| {
        g.nodes
            .iter()
            .find(|n| n.name == name && n.kind != NodeKind::Annotation)
            .unwrap_or_else(|| panic!("no `{name}` in {:?}", names_kinds(&g)))
            .decorators
            .clone()
    };
    assert_eq!(decorators("one"), ["@override"]);
    assert_eq!(decorators("two"), ["@override", "@useResult"]);
    assert!(decorators("none").is_empty());
    assert_eq!(decorators("body"), ["@override"]);
    assert_eq!(decorators("top"), ["@pragma('x')"]);
}

/// FU-B2: a body-less member of a class is a Method owned by the class;
/// a top-level function stays a Function.
#[test]
fn test_dart_abstract_member_is_method_not_function() {
    let g = parse(ABSTRACT_MEMBERS);
    for name in ["one", "two", "none"] {
        let m = node(&g, name, NodeKind::Method);
        assert_eq!(m.owner_class.as_deref(), Some("Cipher"), "{name}");
        assert!(
            !g.nodes
                .iter()
                .any(|n| n.name == name && n.kind == NodeKind::Function),
            "{name} also emitted as Function"
        );
    }
    assert_eq!(node(&g, "body", NodeKind::Method).decorators, ["@override"]);
    assert!(node(&g, "top", NodeKind::Function).owner_class.is_none());
    assert!(!g
        .nodes
        .iter()
        .any(|n| n.name == "top" && n.kind == NodeKind::Method));
}

/// FU-B1 (capability): abstract getter, setter and operator signatures keep
/// their annotations.
#[test]
fn test_dart_abstract_getter_setter_operator_annotations() {
    let g = parse(ABSTRACT_MEMBERS);
    let marked = |dec: &str| {
        g.nodes
            .iter()
            .filter(|n| n.kind != NodeKind::Annotation && n.decorators == [dec])
            .map(|n| n.kind)
            .collect::<Vec<_>>()
    };
    assert_eq!(marked("@a"), [NodeKind::Property], "getter");
    assert_eq!(marked("@b"), [NodeKind::Property], "setter");
    assert_eq!(marked("@o"), [NodeKind::Method], "operator");
}

#[test]
fn test_dart_abstract_members_empty_and_nested_shapes() {
    assert!(parse(b"").nodes.is_empty());
    let g =
        parse(b"abstract class A {\n  @x\n  void m();\n}\nabstract class B {\n  void m();\n}\n");
    let ms: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name == "m" && n.kind == NodeKind::Method)
        .map(|n| (n.owner_class.as_deref(), n.decorators.clone()))
        .collect();
    assert_eq!(ms.len(), 2, "{:?}", names_kinds(&g));
    assert!(ms.contains(&(Some("A"), vec!["@x".to_string()])));
    assert!(ms.contains(&(Some("B"), vec![])));
}
