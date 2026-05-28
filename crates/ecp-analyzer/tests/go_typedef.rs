//! Go defined types and type aliases must surface as `NodeKind::Typedef`.
//!
//! Before this, `type_spec` was captured only when its RHS was a `struct_type`
//! (→ Struct) or `interface_type` (→ Interface); every other named type
//! definition (`type Celsius float64`, `type Handler func(...)`,
//! `type StringMap map[string]string`, `type Bytes []byte`) and every
//! `type_alias` (`type Celsius = float64`) had no capture path, so Go was the
//! only statically-typed language emitting zero Typedef nodes — methods like
//! `func (c Celsius) String()` referenced an owner type with no defining node,
//! leaving the graph incomplete for rename / impact queries.
//!
//! The accuracy guard tests below pin that adding the fallback `type_spec`
//! capture does NOT double-emit struct/interface definitions as Typedef.

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = GoProvider::new().expect("provider");
    p.parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse")
}

fn kinds_of(g: &LocalGraph, name: &str) -> Vec<NodeKind> {
    g.nodes
        .iter()
        .filter(|n| n.name == name)
        .map(|n| n.kind)
        .collect()
}

#[test]
fn defined_type_over_primitive_emits_typedef() {
    let g = parse("package p\n\ntype Celsius float64\n");
    assert_eq!(kinds_of(&g, "Celsius"), vec![NodeKind::Typedef]);
}

#[test]
fn defined_type_over_int_emits_typedef() {
    let g = parse("package p\n\ntype Dir int\n");
    assert_eq!(kinds_of(&g, "Dir"), vec![NodeKind::Typedef]);
}

#[test]
fn defined_func_type_emits_typedef() {
    let g = parse("package p\n\ntype Handler func(int) error\n");
    assert_eq!(kinds_of(&g, "Handler"), vec![NodeKind::Typedef]);
}

#[test]
fn defined_map_type_emits_typedef() {
    let g = parse("package p\n\ntype StringMap map[string]string\n");
    assert_eq!(kinds_of(&g, "StringMap"), vec![NodeKind::Typedef]);
}

#[test]
fn defined_slice_type_emits_typedef() {
    let g = parse("package p\n\ntype Bytes []byte\n");
    assert_eq!(kinds_of(&g, "Bytes"), vec![NodeKind::Typedef]);
}

#[test]
fn type_alias_emits_typedef() {
    let g = parse("package p\n\ntype Celsius = float64\n");
    assert_eq!(kinds_of(&g, "Celsius"), vec![NodeKind::Typedef]);
}

// ── Accuracy guards: the fallback must NOT double-emit struct/interface ──────

#[test]
fn struct_stays_struct_not_double_emitted() {
    let g = parse("package p\n\ntype Foo struct {\n    X int\n}\n");
    assert_eq!(
        kinds_of(&g, "Foo"),
        vec![NodeKind::Struct],
        "struct must emit exactly one Struct node, never a duplicate Typedef"
    );
}

#[test]
fn interface_stays_interface_not_double_emitted() {
    let g = parse("package p\n\ntype Stringer interface {\n    String() string\n}\n");
    assert_eq!(
        kinds_of(&g, "Stringer"),
        vec![NodeKind::Interface],
        "interface must emit exactly one Interface node, never a duplicate Typedef"
    );
}
