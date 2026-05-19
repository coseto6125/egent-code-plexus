//! Move "Named" dimension: `use ... as Alias` declarations emit NodeKind::Typedef.
//!
//! Covers:
//!   - `use 0x1::module as Alias`           (module alias)
//!   - `use 0x1::module::Item as Alias`     (member alias)
//!   - Plain `use 0x1::module::Item`        (no alias → no Typedef node)

use graph_nexus_analyzer::move_lang::parser::MoveProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = MoveProvider::new().expect("MoveProvider init");
    let graph = provider
        .parse_file(Path::new("test.move"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find_typedef<'a>(nodes: &'a [RawNode], name: &str) -> Option<&'a RawNode> {
    nodes.iter().find(|n| n.name == name && n.kind == NodeKind::Typedef)
}

const MODULE_WRAP_OPEN: &str = "module 0x1::T {\n";
const MODULE_WRAP_CLOSE: &str = "\n}";

fn wrap(body: &str) -> String {
    format!("{MODULE_WRAP_OPEN}{body}{MODULE_WRAP_CLOSE}")
}

#[test]
fn use_module_as_emits_typedef() {
    // use 0x1::module as Alias  →  Alias must be NodeKind::Typedef
    let src = wrap("use std::vector as V;");
    let nodes = parse(&src);
    let td = find_typedef(&nodes, "V")
        .unwrap_or_else(|| panic!("`V` typedef not found; nodes: {nodes:#?}"));
    assert_eq!(td.kind, NodeKind::Typedef);
}

#[test]
fn use_member_as_emits_typedef() {
    // use 0x1::module::Item as Alias  →  Alias must be NodeKind::Typedef
    let src = wrap("use aptos_std::ristretto255_bulletproofs::{Self as bulletproofs};");
    let nodes = parse(&src);
    let td = find_typedef(&nodes, "bulletproofs")
        .unwrap_or_else(|| panic!("`bulletproofs` typedef not found; nodes: {nodes:#?}"));
    assert_eq!(td.kind, NodeKind::Typedef);
}

#[test]
fn use_member_simple_as_emits_typedef() {
    // use 0x1::module::Member as Alias
    let src = wrap("use std::option::Option as Opt;");
    let nodes = parse(&src);
    let td = find_typedef(&nodes, "Opt")
        .unwrap_or_else(|| panic!("`Opt` typedef not found; nodes: {nodes:#?}"));
    assert_eq!(td.kind, NodeKind::Typedef);
}

#[test]
fn plain_use_no_alias_does_not_emit_typedef() {
    // use without `as` → no Typedef node emitted
    let src = wrap("use std::vector;");
    let nodes = parse(&src);
    assert!(
        nodes.iter().all(|n| n.kind != NodeKind::Typedef),
        "plain `use std::vector` should not emit Typedef; nodes: {nodes:#?}"
    );
}

#[test]
fn multiple_aliases_coexist() {
    let src = wrap(
        "use std::vector as V;\nuse std::option as O;\nuse std::string::String as Str;",
    );
    let nodes = parse(&src);
    find_typedef(&nodes, "V").unwrap_or_else(|| panic!("`V` missing; nodes: {nodes:#?}"));
    find_typedef(&nodes, "O").unwrap_or_else(|| panic!("`O` missing; nodes: {nodes:#?}"));
    find_typedef(&nodes, "Str").unwrap_or_else(|| panic!("`Str` missing; nodes: {nodes:#?}"));
}
