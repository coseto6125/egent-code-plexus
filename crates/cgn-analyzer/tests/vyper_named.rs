//! Vyper "Named" dimension — alias / typedef detection.
//!
//! Vyper's grammar is minimal and does not parse `import X as Y` or
//! `from X import Y as Z` via tree-sitter nodes. Alias detection is done
//! via source-text scanning in parser.rs.
//!
//! Emits `NodeKind::Typedef` for:
//!   - `import math as m`                         → alias "m"
//!   - `from vyper.interfaces import ERC20 as Token` → alias "Token"
//!
//! Does NOT emit Typedef for bare `from foo import bar` (no `as` clause).

use graph_nexus_analyzer::vyper::parser::VyperProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<(String, NodeKind)> {
    let provider = VyperProvider::new().expect("VyperProvider::new");
    let graph = provider
        .parse_file(Path::new("t.vy"), src.as_bytes())
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
fn test_vyper_import_as_emits_typedef() {
    let nodes = parse("import math as m\n");
    let n = find_node(&nodes, "m");
    assert_eq!(n.1, NodeKind::Typedef, "`import X as Y` must emit NodeKind::Typedef for alias");
}

#[test]
fn test_vyper_from_import_as_emits_typedef() {
    let nodes = parse("from vyper.interfaces import ERC20 as Token\n");
    let n = find_node(&nodes, "Token");
    assert_eq!(n.1, NodeKind::Typedef, "`from X import Y as Z` must emit NodeKind::Typedef for alias");
}

#[test]
fn test_vyper_plain_import_no_typedef() {
    // `from foo import bar` — no `as` clause → no Typedef
    let nodes = parse("import foo\n");
    assert!(
        nodes.iter().all(|(_, k)| *k != NodeKind::Typedef),
        "plain import without `as` must not emit Typedef, got: {nodes:#?}"
    );
}

#[test]
fn test_vyper_from_import_without_alias_no_typedef() {
    // Vyper grammar won't even parse `from X import Y` (no tree-sitter node for it),
    // but even if it produced something, it must not emit Typedef.
    let src = "from foo import bar\n";
    let nodes = parse(src);
    assert!(
        nodes.iter().all(|(_, k)| *k != NodeKind::Typedef),
        "from-import without alias must not emit Typedef, got: {nodes:#?}"
    );
}
