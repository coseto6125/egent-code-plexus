//! Verifies that Dart `typedef` declarations (new-style and old-style)
//! emit NodeKind::Typedef.

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

fn find_typedef(graph: &LocalGraph, name: &str) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| n.name == name && n.kind == NodeKind::Typedef)
}

#[test]
fn new_style_callback_typedef() {
    let g = parse("typedef Callback = void Function(int);");
    assert!(
        find_typedef(&g, "Callback"),
        "`Callback` must be NodeKind::Typedef; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn new_style_generic_typedef() {
    let g = parse("typedef Compare<T> = int Function(T a, T b);");
    assert!(
        find_typedef(&g, "Compare"),
        "`Compare` must be NodeKind::Typedef; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn old_style_typedef() {
    let g = parse("typedef int Comparator(int a, int b);");
    assert!(
        find_typedef(&g, "Comparator"),
        "`Comparator` must be NodeKind::Typedef (old-style); nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn typedef_is_exported_by_default() {
    let g = parse("typedef Callback = void Function(int);");
    let node = g
        .nodes
        .iter()
        .find(|n| n.name == "Callback" && n.kind == NodeKind::Typedef)
        .expect("Callback typedef missing");
    assert!(node.is_exported, "`Callback` must be exported");
}

#[test]
fn private_typedef_is_not_exported() {
    let g = parse("typedef _InternalCb = void Function();");
    let node = g
        .nodes
        .iter()
        .find(|n| n.name == "_InternalCb" && n.kind == NodeKind::Typedef)
        .expect("_InternalCb typedef missing");
    assert!(!node.is_exported, "`_InternalCb` must not be exported");
}
