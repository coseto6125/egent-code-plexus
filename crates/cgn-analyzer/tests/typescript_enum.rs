//! Verifies that TypeScript `enum` declarations emit `NodeKind::Enum`.
//! Covers plain enum, `const enum`, `declare enum`, exported enum, and
//! enum nested inside a namespace.

use graph_nexus_analyzer::typescript::TypeScriptProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = TypeScriptProvider::new().expect("provider");
    p.parse_file(Path::new("test.ts"), src.as_bytes())
        .expect("parse")
}

fn enums(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Enum)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn plain_enum_emits_enum_kind() {
    let g = parse("enum Color { Red, Green, Blue }");
    let defs = enums(&g);
    assert_eq!(defs, vec!["Color"], "nodes: {:?}", g.nodes);
}

#[test]
fn const_enum_emits_enum_kind() {
    let g = parse("const enum Direction { Up, Down, Left, Right }");
    let defs = enums(&g);
    assert_eq!(defs, vec!["Direction"], "nodes: {:?}", g.nodes);
}

#[test]
fn declare_enum_emits_enum_kind() {
    let g = parse("declare enum Status { Active, Idle }");
    let defs = enums(&g);
    assert_eq!(defs, vec!["Status"], "nodes: {:?}", g.nodes);
}

#[test]
fn exported_enum_is_marked_exported() {
    let g = parse("export enum Mode { Read, Write }");
    let defs = enums(&g);
    assert_eq!(defs, vec!["Mode"], "nodes: {:?}", g.nodes);
    let n = g.nodes.iter().find(|n| n.name == "Mode").unwrap();
    assert!(n.is_exported, "expected Mode to be exported");
}

#[test]
fn string_valued_enum_emits_single_enum_node() {
    let g = parse(r#"enum Kind { Foo = "foo", Bar = "bar" }"#);
    let defs = enums(&g);
    assert_eq!(defs, vec!["Kind"], "nodes: {:?}", g.nodes);
}

#[test]
fn enum_inside_namespace_still_emitted() {
    let g = parse("namespace M { export enum Local { A, B } }");
    let defs = enums(&g);
    assert_eq!(defs, vec!["Local"], "nodes: {:?}", g.nodes);
}

#[test]
fn enum_not_double_counted_as_class() {
    let g = parse("enum Solo { Only }");
    let classes: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Class)
        .map(|n| n.name.as_str())
        .collect();
    assert!(
        classes.is_empty(),
        "enum should not emit Class: {:?}",
        g.nodes
    );
}
