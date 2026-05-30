//! `X: TypeAlias = …` (PEP 613) must be classified as Typedef, not Variable —
//! it is a reference target for `ecp find`/impact, not a value binding. Plain
//! and other-annotated module assignments keep the Variable kind.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PythonProvider::new().expect("provider");
    p.parse_file(Path::new("m.py"), src.as_bytes())
        .expect("parse")
}

fn kind_of(g: &LocalGraph, name: &str) -> Option<NodeKind> {
    g.nodes.iter().find(|n| n.name == name).map(|n| n.kind)
}

#[test]
fn bare_typealias_annotation_is_typedef() {
    let g = parse("from typing import TypeAlias\nVector: TypeAlias = list[float]\n");
    assert_eq!(
        kind_of(&g, "Vector"),
        Some(NodeKind::Typedef),
        "TypeAlias-annotated assignment must be Typedef, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn dotted_typealias_annotation_is_typedef() {
    let g = parse("import typing\nMatrix: typing.TypeAlias = list[list[float]]\n");
    assert_eq!(
        kind_of(&g, "Matrix"),
        Some(NodeKind::Typedef),
        "typing.TypeAlias-annotated assignment must be Typedef, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn plain_assignment_stays_variable() {
    let g = parse("count = 5\n");
    assert_eq!(
        kind_of(&g, "count"),
        Some(NodeKind::Variable),
        "plain assignment must stay Variable, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn non_typealias_annotation_stays_variable() {
    let g = parse("count: int = 5\n");
    assert_eq!(
        kind_of(&g, "count"),
        Some(NodeKind::Variable),
        "int-annotated assignment must stay Variable, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn typealias_does_not_double_emit() {
    let g = parse("from typing import TypeAlias\nVector: TypeAlias = list[float]\n");
    let count = g.nodes.iter().filter(|n| n.name == "Vector").count();
    assert_eq!(
        count, 1,
        "Vector must emit exactly one node, nodes: {:?}",
        g.nodes
    );
}
