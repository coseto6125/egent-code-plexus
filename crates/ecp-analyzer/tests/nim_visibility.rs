//! Visibility checks for the Nim provider.
//!
//! In Nim, a symbol is exported iff its name carries the trailing `*` export
//! marker (tree-sitter-nim represents this as an `exported_symbol` node).
//! Underscore-prefixed names have no special meaning in Nim — only the `*`
//! marker determines export status.

use ecp_analyzer::nim::parser::NimProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    NimProvider::new()
        .expect("provider")
        .parse_file(Path::new("test.nim"), src.as_bytes())
        .expect("parse")
}

fn find_exported(g: &LocalGraph, name: &str) -> Option<bool> {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .map(|n| n.is_exported)
}

#[test]
fn exported_proc_has_star() {
    let g = parse("proc foo*() = discard");
    assert_eq!(
        find_exported(&g, "foo"),
        Some(true),
        "`foo*` must be exported"
    );
}

#[test]
fn private_proc_no_star() {
    let g = parse("proc bar() = discard");
    assert_eq!(
        find_exported(&g, "bar"),
        Some(false),
        "`bar` (no `*`) must not be exported"
    );
}

#[test]
fn exported_type_has_star() {
    let g = parse("type Baz* = object");
    assert_eq!(
        find_exported(&g, "Baz"),
        Some(true),
        "`Baz*` must be exported"
    );
}

#[test]
fn private_type_no_star() {
    let g = parse("type Qux = object");
    assert_eq!(
        find_exported(&g, "Qux"),
        Some(false),
        "`Qux` (no `*`) must not be exported"
    );
}
