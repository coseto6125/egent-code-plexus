//! Cairo "Named" dimension — alias/typedef detection.
//!
//! Emits `NodeKind::Typedef` for:
//!   - `use path::to::Item as Alias;`  (aliased import — captures the Alias identifier)
//!   - `type X = Y;`                   (type alias declaration)
//!
//! Plain `use path::to::Item;` (no `as`) must NOT emit Typedef.

use graph_nexus_analyzer::cairo::parser::CairoProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<(String, NodeKind)> {
    let provider = CairoProvider::new().expect("CairoProvider::new");
    let graph = provider
        .parse_file(Path::new("t.cairo"), src.as_bytes())
        .expect("parse_file");
    graph
        .nodes
        .iter()
        .map(|n| (n.name.clone(), n.kind))
        .collect()
}

fn find_node<'a>(nodes: &'a [(String, NodeKind)], name: &str) -> &'a (String, NodeKind) {
    nodes
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("node `{name}` not found in {nodes:#?}"))
}

#[test]
fn test_cairo_use_as_alias_emits_typedef() {
    let nodes = parse("use path::to::Item as Alias;\n");
    let n = find_node(&nodes, "Alias");
    assert_eq!(
        n.1,
        NodeKind::Typedef,
        "aliased use-import must be NodeKind::Typedef"
    );
}

#[test]
fn test_cairo_type_alias_emits_typedef() {
    let nodes = parse("type Meters = u64;\n");
    let n = find_node(&nodes, "Meters");
    assert_eq!(
        n.1,
        NodeKind::Typedef,
        "type alias must be NodeKind::Typedef"
    );
}

#[test]
fn test_cairo_plain_use_not_typedef() {
    let nodes = parse("use path::to::Item;\n");
    // Should not emit any Typedef node (no alias)
    assert!(
        nodes.iter().all(|(_, k)| *k != NodeKind::Typedef),
        "plain use without `as` must not emit Typedef, got: {nodes:#?}"
    );
}

#[test]
fn test_cairo_type_alias_with_generics_emits_typedef() {
    let nodes = parse("type Pair<T> = (T, T);\n");
    let n = find_node(&nodes, "Pair");
    assert_eq!(
        n.1,
        NodeKind::Typedef,
        "generic type alias must be NodeKind::Typedef"
    );
}

#[test]
fn test_cairo_both_typedef_forms_coexist() {
    let src = "use core::felt252 as Felt;\ntype Hash = felt252;\n";
    let nodes = parse(src);
    let felt = find_node(&nodes, "Felt");
    assert_eq!(felt.1, NodeKind::Typedef);
    let hash = find_node(&nodes, "Hash");
    assert_eq!(hash.1, NodeKind::Typedef);
}
