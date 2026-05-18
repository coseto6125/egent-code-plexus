//! Crystal "Named" dimension: `alias X = Y` declarations emit NodeKind::Typedef.
//!
//! Crystal grammar node: `alias` with field `name: constant`.

use graph_nexus_analyzer::crystal::parser::CrystalProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    CrystalProvider::new()
        .expect("provider")
        .parse_file(Path::new("test.cr"), src.as_bytes())
        .expect("parse")
}

fn find_typedef(g: &LocalGraph, name: &str) -> Option<bool> {
    g.nodes
        .iter()
        .find(|n| n.name == name && n.kind == NodeKind::Typedef)
        .map(|n| n.is_exported)
}

#[test]
fn simple_type_alias_emits_typedef() {
    // alias RouteHandler = HTTP::Server::Context -> String
    let g = parse("alias RouteHandler = HTTP::Server::Context -> String");
    assert!(
        find_typedef(&g, "RouteHandler").is_some(),
        "`RouteHandler` must be NodeKind::Typedef; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn union_type_alias_emits_typedef() {
    // alias AllParamTypes = String | Int64 | Bool
    let g = parse("alias AllParamTypes = String | Int64 | Bool");
    assert!(
        find_typedef(&g, "AllParamTypes").is_some(),
        "`AllParamTypes` must be NodeKind::Typedef; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn alias_inside_module_emits_typedef() {
    let g = parse(
        "module Kemal\n  alias WSHandler = HTTP::WebSocket, HTTP::Server::Context ->\nend",
    );
    assert!(
        find_typedef(&g, "WSHandler").is_some(),
        "`WSHandler` inside module must be NodeKind::Typedef; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn alias_is_exported_by_default() {
    let g = parse("alias Callback = Proc(Int32, Nil)");
    let exported = find_typedef(&g, "Callback")
        .unwrap_or_else(|| panic!("`Callback` typedef missing; nodes: {:#?}", g.nodes));
    assert!(exported, "`Callback` alias must be exported (default public)");
}

#[test]
fn multiple_aliases_coexist() {
    let g = parse(
        "alias RouteHandler = HTTP::Server::Context -> String\nalias FilterHandler = HTTP::Server::Context -> String",
    );
    assert!(
        find_typedef(&g, "RouteHandler").is_some(),
        "`RouteHandler` missing; nodes: {:#?}",
        g.nodes
    );
    assert!(
        find_typedef(&g, "FilterHandler").is_some(),
        "`FilterHandler` missing; nodes: {:#?}",
        g.nodes
    );
}
