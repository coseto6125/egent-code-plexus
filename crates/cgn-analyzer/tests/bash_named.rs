//! Bash "Named" dimension — `alias NAME=...` emits `NodeKind::Typedef`.

use cgn_analyzer::bash::parser::BashProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<(String, NodeKind)> {
    let provider = BashProvider::new().expect("BashProvider::new");
    let graph = provider
        .parse_file(Path::new("t.sh"), src.as_bytes())
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
fn test_bash_alias_single_quoted_emits_typedef() {
    let nodes = parse("alias ll='ls -la'\n");
    let n = find_node(&nodes, "ll");
    assert_eq!(n.1, NodeKind::Typedef, "alias with single-quoted RHS must be Typedef");
}

#[test]
fn test_bash_alias_double_quoted_emits_typedef() {
    let nodes = parse("alias gs=\"git status\"\n");
    let n = find_node(&nodes, "gs");
    assert_eq!(n.1, NodeKind::Typedef);
}

#[test]
fn test_bash_alias_unquoted_emits_typedef() {
    let nodes = parse("alias la=ls\n");
    let n = find_node(&nodes, "la");
    assert_eq!(n.1, NodeKind::Typedef);
}

#[test]
fn test_bash_function_not_confused_with_alias() {
    let nodes = parse("myfunc() { echo hello; }\nalias greet='echo hi'\n");
    let f = find_node(&nodes, "myfunc");
    assert_eq!(f.1, NodeKind::Function, "function must stay Function");
    let a = find_node(&nodes, "greet");
    assert_eq!(a.1, NodeKind::Typedef, "alias must be Typedef");
}

#[test]
fn test_bash_multiple_aliases_all_typedef() {
    let src = "alias ll='ls -la'\nalias gs=\"git status\"\nalias reload=\"source ~/.bashrc\"\n";
    let nodes = parse(src);
    for name in ["ll", "gs", "reload"] {
        let n = find_node(&nodes, name);
        assert_eq!(n.1, NodeKind::Typedef, "alias `{name}` must be Typedef");
    }
}
