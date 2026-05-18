//! `pub` keyword visibility checks for the Cairo provider.
//!
//! The vendored tree-sitter-cairo grammar (v0.0.1) does not expose a named
//! `visibility_modifier` AST node — `pub` is an anonymous token absent from
//! the grammar entirely in this version. The provider therefore falls back to
//! a source-text scan: a declaration is exported when the raw bytes of its
//! root span start with `pub ` (after any attribute lines).
//!
//! Convention: default (no `pub`) = not exported; `pub` prefix = exported.

use graph_nexus_analyzer::cairo::parser::CairoProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CairoProvider::new().expect("CairoProvider init");
    let graph = provider
        .parse_file(Path::new("test.cairo"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn pub_function_is_exported() {
    let nodes = parse("pub fn foo() {}");
    let foo = find(&nodes, "foo", NodeKind::Function);
    assert!(foo.is_exported, "`pub fn foo` must be exported");
}

#[test]
fn private_function_is_not_exported() {
    let nodes = parse("fn bar() {}");
    let bar = find(&nodes, "bar", NodeKind::Function);
    assert!(!bar.is_exported, "`fn bar` (no pub) must not be exported");
}

#[test]
fn pub_struct_is_exported() {
    let nodes = parse("pub struct Point { x: felt252, y: felt252 }");
    let pt = find(&nodes, "Point", NodeKind::Class);
    assert!(pt.is_exported, "`pub struct Point` must be exported");
}

#[test]
fn private_struct_is_not_exported() {
    let nodes = parse("struct Hidden { v: felt252 }");
    let h = find(&nodes, "Hidden", NodeKind::Class);
    assert!(
        !h.is_exported,
        "`struct Hidden` (no pub) must not be exported"
    );
}

#[test]
fn pub_mod_is_exported() {
    // Module with an inline body
    let nodes = parse("pub mod mymod { fn inner() {} }");
    let m = find(&nodes, "mymod", NodeKind::Class);
    assert!(m.is_exported, "`pub mod mymod` must be exported");
}

#[test]
fn private_mod_is_not_exported() {
    let nodes = parse("mod secret { fn inner() {} }");
    let m = find(&nodes, "secret", NodeKind::Class);
    assert!(!m.is_exported, "`mod secret` (no pub) must not be exported");
}

#[test]
fn pub_const_is_exported() {
    let nodes = parse("pub const MAX: felt252 = 100;");
    let c = find(&nodes, "MAX", NodeKind::Const);
    assert!(c.is_exported, "`pub const MAX` must be exported");
}

#[test]
fn private_const_is_not_exported() {
    let nodes = parse("const LIMIT: felt252 = 42;");
    let c = find(&nodes, "LIMIT", NodeKind::Const);
    assert!(
        !c.is_exported,
        "`const LIMIT` (no pub) must not be exported"
    );
}
