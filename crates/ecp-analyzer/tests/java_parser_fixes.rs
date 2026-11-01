//! `JavaProvider` visibility of interface members. A member declared in an
//! `interface` or `@interface` body without an access modifier is implicitly
//! `public`; a `private` interface method is not.

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    JavaProvider::new()
        .expect("JavaProvider::new")
        .parse_file(Path::new("Test.java"), src.as_bytes())
        .expect("parse_file")
}

/// `is_exported` of the one node with this name and kind.
fn exported(g: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    let hits: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name == name && n.kind == kind)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "{name} {kind:?} in {:?}",
        g.nodes
            .iter()
            .map(|n| (&n.name, n.kind))
            .collect::<Vec<_>>()
    );
    hits[0].is_exported
}

#[test]
fn test_java_interface_members_without_modifier_are_exported() {
    let g = parse(
        "public interface Api {\n\
         \x20   int MAX = 1;\n\
         \x20   void run();\n\
         \x20   default void helper() {}\n\
         \x20   static void make() {}\n\
         \x20   class Impl {}\n\
         \x20   interface Inner { void deep(); }\n\
         \x20   enum Mode { A }\n\
         }\n",
    );
    assert!(exported(&g, "run", NodeKind::Method));
    assert!(exported(&g, "helper", NodeKind::Method));
    assert!(exported(&g, "make", NodeKind::Method));
    assert!(exported(&g, "MAX", NodeKind::Property));
    assert!(exported(&g, "Impl", NodeKind::Class));
    assert!(exported(&g, "Inner", NodeKind::Interface));
    assert!(exported(&g, "deep", NodeKind::Method));
    assert!(exported(&g, "Mode", NodeKind::Enum));
}

#[test]
fn test_java_private_interface_method_stays_unexported() {
    let g = parse(
        "public interface Api {\n\
         \x20   void open();\n\
         \x20   private void hidden() {}\n\
         \x20   private static void hiddenStatic() {}\n\
         }\n",
    );
    assert!(exported(&g, "open", NodeKind::Method));
    assert!(!exported(&g, "hidden", NodeKind::Method));
    assert!(!exported(&g, "hiddenStatic", NodeKind::Method));
}

#[test]
fn test_java_package_private_interface_members_follow_the_interface() {
    let g = parse("interface Local {\n    void run();\n    int N = 2;\n}\n");
    assert!(!exported(&g, "Local", NodeKind::Interface));
    assert!(!exported(&g, "run", NodeKind::Method));
    assert!(!exported(&g, "N", NodeKind::Property));
}

#[test]
fn test_java_class_members_without_modifier_stay_unexported() {
    let g = parse(
        "public class Plain {\n\
         \x20   void pkg() {}\n\
         \x20   int field;\n\
         \x20   public void open() {}\n\
         \x20   class Nested {}\n\
         }\n",
    );
    assert!(!exported(&g, "pkg", NodeKind::Method));
    assert!(!exported(&g, "field", NodeKind::Property));
    assert!(exported(&g, "open", NodeKind::Method));
    assert!(!exported(&g, "Nested", NodeKind::Class));
}

/// FU-B3: `enum` and `@interface` follow the same visibility rule as a
/// class, and members of an exported `@interface` stay exported.
#[test]
fn test_java_public_enum_and_annotation_type_are_exported() {
    let g = parse(
        "public class Outer {\n\
         \x20   public enum Mode { A }\n\
         \x20   public @interface Require {\n\
         \x20       String value() default \"\";\n\
         \x20       int count();\n\
         \x20   }\n\
         }\n\
         public enum Top { X }\n",
    );
    assert!(exported(&g, "Mode", NodeKind::Enum));
    assert!(exported(&g, "Top", NodeKind::Enum));
    assert!(exported(&g, "Require", NodeKind::Annotation));
    assert!(exported(&g, "value", NodeKind::Method));
    assert!(exported(&g, "count", NodeKind::Method));
}

#[test]
fn test_java_package_private_enum_and_annotation_type_stay_unexported() {
    let g = parse(
        "enum Hidden { A }\n\
         @interface Local { int n(); }\n\
         public class Pub {\n\
         \x20   enum Inner { B }\n\
         }\n",
    );
    assert!(!exported(&g, "Hidden", NodeKind::Enum));
    assert!(!exported(&g, "Local", NodeKind::Annotation));
    assert!(!exported(&g, "n", NodeKind::Method));
    assert!(!exported(&g, "Inner", NodeKind::Enum));
}

#[test]
fn test_java_same_name_enum_in_two_scopes_follows_each_owner() {
    let g = parse(
        "public class A { public enum Kind { X } }\n\
         class B { enum Kind { Y } }\n",
    );
    let flags: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name == "Kind" && n.kind == NodeKind::Enum)
        .map(|n| n.is_exported)
        .collect();
    assert_eq!(flags, [true, false]);
}

#[test]
fn test_java_empty_file_has_no_exported_nodes() {
    assert!(parse("").nodes.is_empty());
}

/// Enum constants are implicitly `public`, so they are exported exactly when
/// their enum is: a `public` enum, a nested `public` enum, an enum nested in an
/// interface (implicitly public), and a constant with arguments or a body.
/// A package-private enum keeps its constants unexported, and an enum method
/// still follows its own modifier.
#[test]
fn test_java_enum_constants_follow_the_enum_export() {
    let g = parse(
        "public enum Color {\n    RED, GREEN(2), BLUE { void f() {} };\n    Color() {}\n    Color(int n) {}\n    void pkg() {}\n    public void open() {}\n}\n\
         enum Hidden { ONE, TWO }\n\
         interface Api { enum Mode { FAST } }\n\
         public class Outer { public enum In { A } private enum Priv { P } }\n",
    );
    for name in ["RED", "GREEN", "BLUE"] {
        assert!(exported(&g, name, NodeKind::EnumVariant), "{name}");
    }
    assert!(!exported(&g, "ONE", NodeKind::EnumVariant));
    assert!(!exported(&g, "TWO", NodeKind::EnumVariant));
    assert!(!exported(&g, "pkg", NodeKind::Method));
    assert!(exported(&g, "open", NodeKind::Method));
    assert!(!exported(&g, "Hidden", NodeKind::Enum));
    assert!(exported(&g, "FAST", NodeKind::EnumVariant));
    assert!(exported(&g, "A", NodeKind::EnumVariant));
    assert!(!exported(&g, "P", NodeKind::EnumVariant));
}

#[test]
fn test_java_empty_enum_has_no_constants() {
    let g = parse("public enum Empty {}\n");
    assert!(g.nodes.iter().all(|n| n.kind != NodeKind::EnumVariant));
}
