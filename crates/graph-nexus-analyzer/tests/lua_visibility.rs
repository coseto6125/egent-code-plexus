//! Lua visibility checks: top-level `function` vs `local function`.
//!
//! Lua has no explicit export keyword. The convention is:
//!   * `function foo()` (no `local`) → exported (is_exported = true)
//!   * `local function foo()` → file-private (is_exported = false)
//!
//! The grammar (tree-sitter-lua 0.5.0) aliases both as `function_declaration`,
//! but the local form is wrapped in a `declaration` node under the
//! `local_declaration` field, enabling query-level discrimination.

use graph_nexus_analyzer::lua::parser::LuaProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = LuaProvider::new().expect("LuaProvider init");
    let graph = provider
        .parse_file(Path::new("test.lua"), src.as_bytes())
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
fn global_function_is_exported() {
    let nodes = parse("function pub() end");
    let node = find(&nodes, "pub", NodeKind::Function);
    assert!(node.is_exported, "`pub` (global function) must be exported");
}

#[test]
fn local_function_is_not_exported() {
    let nodes = parse("local function priv() end");
    let node = find(&nodes, "priv", NodeKind::Function);
    assert!(
        !node.is_exported,
        "`priv` (local function) must not be exported"
    );
}

#[test]
fn mixed_local_and_global_functions() {
    let src = "local function priv() end\nfunction pub() end";
    let nodes = parse(src);

    let pub_node = find(&nodes, "pub", NodeKind::Function);
    assert!(pub_node.is_exported, "`pub` must be exported");

    let priv_node = find(&nodes, "priv", NodeKind::Function);
    assert!(!priv_node.is_exported, "`priv` must not be exported");
}

#[test]
fn multiple_local_functions_all_private() {
    let src = "local function a() end\nlocal function b() end";
    let nodes = parse(src);

    let a = find(&nodes, "a", NodeKind::Function);
    assert!(!a.is_exported, "`a` must not be exported");

    let b = find(&nodes, "b", NodeKind::Function);
    assert!(!b.is_exported, "`b` must not be exported");
}
