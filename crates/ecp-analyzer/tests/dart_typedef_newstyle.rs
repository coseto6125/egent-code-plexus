//! Dart `typedef Foo = void Function(...)` (new-style) must emit a Typedef
//! RawNode.
//!
//! tree-sitter-dart mis-parses new-style typedefs as
//! `top_level_variable_declaration` with type text "typedef". Previously the
//! parser detected this and silently skipped — but the `(type_alias ...)`
//! query path only matches old-style `typedef int Compare(int, int)`, so
//! new-style typedefs disappeared entirely.
//!
//! Fix: synthesize a Typedef RawNode from the misparsed node.

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = DartProvider::new().expect("DartProvider init");
    p.parse_file(Path::new("t.dart"), src.as_bytes())
        .expect("parse_file")
}

fn typedefs(g: &LocalGraph) -> Vec<&RawNode> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Typedef)
        .collect()
}

#[test]
fn newstyle_function_typedef_emits_typedef() {
    let g = parse("typedef Callback = void Function(int);\n");
    let tds = typedefs(&g);
    assert_eq!(tds.len(), 1, "expected 1 Typedef, got nodes={:?}", g.nodes);
    assert_eq!(tds[0].name, "Callback");
}

#[test]
fn newstyle_generic_typedef_emits_typedef() {
    let g =
        parse("typedef MasonGeneratorFromBundle = Future<MasonGenerator> Function(MasonBundle);\n");
    let tds = typedefs(&g);
    assert_eq!(tds.len(), 1);
    assert_eq!(tds[0].name, "MasonGeneratorFromBundle");
}

#[test]
fn private_newstyle_typedef_emits_unexported_typedef() {
    let g = parse("typedef _Handler = Future<dynamic> Function();\n");
    let tds = typedefs(&g);
    assert_eq!(tds.len(), 1);
    assert_eq!(tds[0].name, "_Handler");
    assert!(
        !tds[0].is_exported,
        "underscore prefix should mark as not exported"
    );
}

#[test]
fn newstyle_typedef_does_not_double_emit_as_variable() {
    let g = parse("typedef Callback = void Function(int);\n");
    let variables: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variable)
        .collect();
    assert!(
        variables.is_empty(),
        "typedef misparse must not leak as Variable, got {:?}",
        variables
    );
}

#[test]
fn multiple_newstyle_typedefs_each_emit() {
    let g = parse(
        "typedef A = void Function();\n\
         typedef B = int Function(int);\n\
         typedef C = String Function(String);\n",
    );
    let names: Vec<&str> = typedefs(&g).iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"A"));
    assert!(names.contains(&"B"));
    assert!(names.contains(&"C"));
}

#[test]
fn variable_with_normal_type_is_not_typedef() {
    // Regression guard for the skip logic: `double pi = 3.14` must still
    // become Variable, not Typedef.
    let g = parse("double pi = 3.14;\n");
    let tds = typedefs(&g);
    assert!(
        tds.is_empty(),
        "non-typedef top-level var leaked as Typedef: {:?}",
        g.nodes
    );
    let vars: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variable)
        .collect();
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0].name, "pi");
}
