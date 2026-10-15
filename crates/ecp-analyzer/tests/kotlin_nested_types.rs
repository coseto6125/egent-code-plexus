//! FU-2026-10-15-273ad5f60a1d. tree-sitter-kotlin turns a class body whose last
//! member shares a line with the closing `}` into an ERROR node, which dropped
//! the enclosing classes and `object`s and every owner below them. The parser
//! now re-parses with a separator before such a `}`. Multi-line sources are
//! guards: they parsed correctly before. Contract under test: every declared
//! class/object is a node and each member's `owner_class` is its tightest
//! enclosing class, the way the Java parser records it.
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    KotlinProvider::new()
        .expect("KotlinProvider::new")
        .parse_file(Path::new("test.kt"), source.as_bytes())
        .expect("parse_file")
}

/// `(kind, owner_class, 0-based start row)` of the only node called `name`.
fn shape(g: &LocalGraph, name: &str) -> (NodeKind, Option<String>, u32) {
    let hits: Vec<_> = g.nodes.iter().filter(|n| n.name == name).collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one node named {name}: {:?}",
        g.nodes
            .iter()
            .map(|n| (&n.name, n.kind))
            .collect::<Vec<_>>()
    );
    (hits[0].kind, hits[0].owner_class.clone(), hits[0].span.0)
}

fn owned(owner: &str) -> Option<String> {
    Some(owner.to_string())
}

#[test]
fn test_parse_file_nested_class_one_line_records_outer_and_owner() {
    let g = parse("class Outer2 { class Helper { fun run() {} } }\n");
    assert_eq!(shape(&g, "Outer2"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "Helper"), (NodeKind::Class, owned("Outer2"), 0));
    assert_eq!(shape(&g, "run"), (NodeKind::Method, owned("Helper"), 0));
}

#[test]
fn test_parse_file_nested_class_multi_line_records_outer_and_owner() {
    let g = parse("class Outer2 {\n    class Helper {\n        fun run() {}\n    }\n}\n");
    assert_eq!(shape(&g, "Outer2"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "Helper"), (NodeKind::Class, owned("Outer2"), 1));
    assert_eq!(shape(&g, "run"), (NodeKind::Method, owned("Helper"), 2));
}

#[test]
fn test_parse_file_three_level_nesting_one_line_chains_owners() {
    let g = parse("class A { class B { class C { fun deep() {} } } }\n");
    assert_eq!(shape(&g, "A"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "B"), (NodeKind::Class, owned("A"), 0));
    assert_eq!(shape(&g, "C"), (NodeKind::Class, owned("B"), 0));
    assert_eq!(shape(&g, "deep"), (NodeKind::Method, owned("C"), 0));
}

#[test]
fn test_parse_file_three_level_nesting_multi_line_chains_owners() {
    let g = parse("class A {\n  class B {\n    class C {\n      fun deep() {}\n    }\n  }\n}\n");
    assert_eq!(shape(&g, "B"), (NodeKind::Class, owned("A"), 1));
    assert_eq!(shape(&g, "C"), (NodeKind::Class, owned("B"), 2));
    assert_eq!(shape(&g, "deep"), (NodeKind::Method, owned("C"), 3));
}

#[test]
fn test_parse_file_inner_class_one_line_has_owner() {
    let g = parse("class A { inner class B { fun p() {} } }\n");
    assert_eq!(shape(&g, "A"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "B"), (NodeKind::Class, owned("A"), 0));
    assert_eq!(shape(&g, "p"), (NodeKind::Method, owned("B"), 0));
}

#[test]
fn test_parse_file_nested_object_one_line_has_owner() {
    let g = parse("class A { object Inner { fun io() {} } }\n");
    assert_eq!(shape(&g, "A"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "Inner"), (NodeKind::Class, owned("A"), 0));
    assert_eq!(shape(&g, "io"), (NodeKind::Method, owned("Inner"), 0));
}

#[test]
fn test_parse_file_unnamed_companion_one_line_members_belong_to_class() {
    let g = parse("class A { companion object { fun f() {} } }\n");
    assert_eq!(shape(&g, "A"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "f"), (NodeKind::Method, owned("A"), 0));
}

#[test]
fn test_parse_file_named_companion_one_line_members_belong_to_class() {
    let g = parse("class A { companion object Fac { fun f() {} } }\n");
    assert_eq!(shape(&g, "A"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "f"), (NodeKind::Method, owned("A"), 0));
}

#[test]
fn test_parse_file_named_companion_multi_line_members_belong_to_class() {
    let g = parse("class A {\n  companion object Fac {\n    fun f() {}\n  }\n}\n");
    assert_eq!(shape(&g, "A"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "f"), (NodeKind::Method, owned("A"), 2));
}

#[test]
fn test_parse_file_object_after_companion_class_one_line_is_recorded() {
    let g = parse("class A { companion object { fun f() {} } }\nobject Named { fun x() {} }\n");
    assert_eq!(shape(&g, "A"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "Named"), (NodeKind::Class, None, 1));
    assert_eq!(shape(&g, "x"), (NodeKind::Method, owned("Named"), 1));
}

#[test]
fn test_parse_file_object_after_companion_class_multi_line_is_recorded() {
    let g = parse(
        "class A {\n  companion object {\n    fun f() {}\n  }\n}\nobject Named {\n  fun x() {}\n}\n",
    );
    assert_eq!(shape(&g, "Named"), (NodeKind::Class, None, 5));
    assert_eq!(shape(&g, "x"), (NodeKind::Method, owned("Named"), 6));
}

#[test]
fn test_parse_file_two_top_level_objects_one_line_each_are_recorded() {
    let g = parse("object O1 { fun a() {} }\nobject O2 { fun b() {} }\n");
    assert_eq!(shape(&g, "O1"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "a"), (NodeKind::Method, owned("O1"), 0));
    assert_eq!(shape(&g, "O2"), (NodeKind::Class, None, 1));
    assert_eq!(shape(&g, "b"), (NodeKind::Method, owned("O2"), 1));
}

#[test]
fn test_parse_file_enum_with_nested_class_one_line_records_member() {
    let g = parse("enum class E { X, Y; class N { fun en() {} } }\n");
    assert_eq!(shape(&g, "E"), (NodeKind::Enum, None, 0));
    let (kind, _, row) = shape(&g, "N");
    assert_eq!((kind, row), (NodeKind::Class, 0));
    assert_eq!(shape(&g, "en"), (NodeKind::Method, owned("N"), 0));
}

#[test]
fn test_parse_file_interface_with_nested_class_one_line_has_owner() {
    let g = parse("interface I { class IN { fun inn() {} } }\n");
    assert_eq!(shape(&g, "I"), (NodeKind::Interface, None, 0));
    assert_eq!(shape(&g, "IN"), (NodeKind::Class, owned("I"), 0));
    assert_eq!(shape(&g, "inn"), (NodeKind::Method, owned("IN"), 0));
}

#[test]
fn test_parse_file_empty_body_and_bodiless_classes_are_recorded() {
    let g = parse("class A {}\nclass B\nclass C {\n}\n");
    assert_eq!(shape(&g, "A"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "B"), (NodeKind::Class, None, 1));
    assert_eq!(shape(&g, "C"), (NodeKind::Class, None, 2));
}

#[test]
fn test_parse_file_only_companion_bearing_class_one_line_is_recorded() {
    let g = parse("class Only { companion object { const val K = 1 } }");
    assert_eq!(shape(&g, "Only"), (NodeKind::Class, None, 0));
}

#[test]
fn test_parse_file_empty_source_returns_no_nodes() {
    assert!(parse("").nodes.is_empty());
    assert!(parse("   \n").nodes.is_empty());
}

/// The recovery overwrites a blank, never inserts one, and node data reads
/// the unpatched source. Line 0 holds a nested class body that closes on
/// its last member's line, so it needs the recovery, and a `}` after a blank
/// inside a string literal, which the recovery patches for the re-parse
/// too. Every class and member is recorded with its owner, `g` keeps its
/// column on that line, and `Outer`'s hash covers the bytes the file holds.
#[test]
fn test_parse_file_closer_in_string_keeps_columns_and_source_bytes() {
    let line = "class Outer { class A { fun f() { val s = \"a }\" }; fun g() {} } }";
    let source = format!("{line}\nclass B {{\n  fun h() {{}}\n}}\n");
    let graph = parse(&source);
    assert_eq!(shape(&graph, "Outer"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&graph, "A"), (NodeKind::Class, owned("Outer"), 0));
    assert_eq!(shape(&graph, "f"), (NodeKind::Method, owned("A"), 0));
    assert_eq!(shape(&graph, "g"), (NodeKind::Method, owned("A"), 0));
    assert_eq!(shape(&graph, "B"), (NodeKind::Class, None, 1));
    assert_eq!(shape(&graph, "h"), (NodeKind::Method, owned("B"), 2));
    let node = |name: &str| graph.nodes.iter().find(|n| n.name == name).unwrap();
    let column = line.find("fun g").unwrap() as u32;
    assert_eq!(
        (node("g").span.0, node("g").span.1),
        (0, column),
        "a byte inserted before `g` would move it"
    );
    let outer = node("Outer");
    assert_eq!((outer.span.0, outer.span.2), (0, 0));
    assert_eq!(
        outer.content_hash,
        ecp_core::uid::xxh3_64_bytes(
            &line.as_bytes()[outer.span.1 as usize..outer.span.3 as usize]
        ),
        "the hash reads the unpatched source"
    );
}

/// Upstream fwcd/tree-sitter-kotlin #280: a declaration with two or more
/// modifiers followed by another declaration parsed, without an ERROR node,
/// as an infix expression, so `First` was never a class.
#[test]
fn test_parse_file_multi_modifier_class_before_class_is_recorded() {
    let g = parse("internal open class First {}\nclass Second {}\n");
    assert_eq!(shape(&g, "First"), (NodeKind::Class, None, 0));
    assert_eq!(shape(&g, "Second"), (NodeKind::Class, None, 1));
}
