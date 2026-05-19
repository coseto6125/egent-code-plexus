//! Lua "Named" dimension — alias/typedef binding detection.
//!
//! Emits `NodeKind::Typedef` for:
//!   - `local M = require("foo")`  (module alias)
//!   - `local Alias = Tbl.Nested.Type`  (dotted-path alias)
//!
//! Does NOT emit Typedef for plain locals with literal RHS (`local x = 42`).

use cgn_analyzer::lua::parser::LuaProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<(String, NodeKind)> {
    let provider = LuaProvider::new().expect("LuaProvider::new");
    let graph = provider
        .parse_file(Path::new("t.lua"), src.as_bytes())
        .expect("parse_file");
    graph.nodes.iter().map(|n| (n.name.clone(), n.kind)).collect()
}

fn find_node<'a>(nodes: &'a [(String, NodeKind)], name: &str) -> &'a (String, NodeKind) {
    nodes
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("node `{name}` not found in {nodes:#?}"))
}

#[test]
fn test_lua_require_alias_emits_typedef() {
    let nodes = parse("local M = require('mymodule')\n");
    let n = find_node(&nodes, "M");
    assert_eq!(n.1, NodeKind::Typedef, "require alias must be NodeKind::Typedef");
}

#[test]
fn test_lua_dotted_path_alias_emits_typedef() {
    let nodes = parse("local Alias = SomeTable.Nested.Type\n");
    let n = find_node(&nodes, "Alias");
    assert_eq!(n.1, NodeKind::Typedef, "dot-path alias must be NodeKind::Typedef");
}

#[test]
fn test_lua_plain_literal_local_not_typedef() {
    let nodes = parse("local x = 42\n");
    // Should be Const (or absent from nodes), never Typedef
    if let Some(n) = nodes.iter().find(|(name, _)| name == "x") {
        assert_ne!(n.1, NodeKind::Typedef, "plain literal local must not be Typedef");
    }
}

#[test]
fn test_lua_function_not_confused_with_typedef() {
    let nodes = parse("local function foo() end\nlocal M = require('bar')\n");
    let f = find_node(&nodes, "foo");
    assert_eq!(f.1, NodeKind::Function, "local function must stay Function");
    let m = find_node(&nodes, "M");
    assert_eq!(m.1, NodeKind::Typedef);
}

#[test]
fn test_lua_multiple_typedefs_coexist() {
    let src = "local M = require('mod')\nlocal Alias = A.B.C\nlocal x = 1\n";
    let nodes = parse(src);
    let m = find_node(&nodes, "M");
    assert_eq!(m.1, NodeKind::Typedef);
    let alias = find_node(&nodes, "Alias");
    assert_eq!(alias.1, NodeKind::Typedef);
    // x should not be Typedef
    if let Some(n) = nodes.iter().find(|(name, _)| name == "x") {
        assert_ne!(n.1, NodeKind::Typedef, "x must not be Typedef");
    }
}
