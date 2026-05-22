//! Regression: nested (inner) Java classes with the same name but different outer
//! classes must each receive a distinct `owner_class`, preventing uid collisions
//! (uid = kind + path + owner_class + name).

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = JavaProvider::new().expect("JavaProvider::new");
    provider
        .parse_file(Path::new("Test.java"), source.as_bytes())
        .expect("parse_file")
}

fn owner_of<'a>(g: &'a LocalGraph, name: &str) -> Vec<Option<&'a str>> {
    g.nodes
        .iter()
        .filter(|n| n.name == name)
        .map(|n| n.owner_class.as_deref())
        .collect()
}

/// Two sibling top-level classes each containing a static nested class named
/// `Builder` must each emit with a distinct owner_class.
#[test]
fn sibling_top_level_classes_nested_same_name() {
    let src = r#"
public class MapMaker {
    static class Builder {}
}
class OtherFactory {
    static class Builder {}
}
"#;
    let g = parse(src);
    let builders: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Class && n.name == "Builder")
        .collect();
    assert_eq!(builders.len(), 2, "both Builder classes must be emitted");
    let owners: Vec<_> = builders
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"MapMaker"),
        "one Builder must own MapMaker; owners={owners:?}"
    );
    assert!(
        owners.contains(&"OtherFactory"),
        "one Builder must own OtherFactory; owners={owners:?}"
    );
}

/// Deep nesting: inner class inside outer class must get outer as owner_class.
#[test]
fn deeply_nested_inner_class_gets_owner() {
    let src = r#"
public class Outer {
    static class Middle {
        static class Inner {}
    }
}
"#;
    let g = parse(src);
    let middle = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == "Middle")
        .expect("Middle node");
    assert_eq!(
        middle.owner_class.as_deref(),
        Some("Outer"),
        "Middle.owner_class must be Outer; got {:?}",
        middle.owner_class
    );
    let inner = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == "Inner")
        .expect("Inner node");
    assert_eq!(
        inner.owner_class.as_deref(),
        Some("Middle"),
        "Inner.owner_class must be Middle; got {:?}",
        inner.owner_class
    );
}

/// Top-level class must NOT get an owner_class (it is not nested).
#[test]
fn top_level_class_has_no_owner() {
    let src = "public class TopLevel {}\n";
    let g = parse(src);
    let top = g
        .nodes
        .iter()
        .find(|n| n.name == "TopLevel")
        .expect("TopLevel");
    assert!(
        top.owner_class.is_none(),
        "TopLevel must have no owner_class; got {:?}",
        top.owner_class
    );
}

/// Guava-style file: multiple static nested classes across nested hierarchy
/// produce distinct owners (spot-check `Builder` inside two nested classes).
#[test]
fn guava_style_nested_classes_distinct_owners() {
    let src = r#"
public class MapMakerInternalMap {
    static class StrongKeyEntry {
        static class Builder {}
    }
    static class WeakKeyEntry {
        static class Builder {}
    }
}
"#;
    let g = parse(src);
    let builders: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Class && n.name == "Builder")
        .collect();
    assert_eq!(
        builders.len(),
        2,
        "both Builders must be emitted: {builders:?}"
    );
    let owners: Vec<Option<&str>> = builders.iter().map(|n| n.owner_class.as_deref()).collect();
    assert!(
        owners.contains(&Some("StrongKeyEntry")),
        "one Builder must own StrongKeyEntry; owners={owners:?}"
    );
    assert!(
        owners.contains(&Some("WeakKeyEntry")),
        "one Builder must own WeakKeyEntry; owners={owners:?}"
    );
    // Outer class must have no owner (it's top-level).
    let outer_owners = owner_of(&g, "MapMakerInternalMap");
    assert!(
        outer_owners.iter().all(|o| o.is_none()),
        "MapMakerInternalMap must have no owner; got {outer_owners:?}"
    );
}
