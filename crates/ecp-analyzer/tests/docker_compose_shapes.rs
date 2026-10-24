//! Compose field shapes the provider reads: `build:` as a mapping with
//! `context`, `depends_on:` as a mapping, and a blind spot for every shape it
//! does not read, so an empty result is never mistaken for "no dependency".

use ecp_analyzer::docker_compose::parser::DockerComposeProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    DockerComposeProvider::new()
        .expect("compose provider construction")
        .parse_file(Path::new("docker-compose.yml"), source.as_bytes())
        .expect("parse compose yaml")
}

fn build_sources(g: &LocalGraph) -> Vec<&str> {
    g.imports.iter().map(|i| i.source.as_str()).collect()
}

fn calls_of<'g>(g: &'g LocalGraph, service: &str) -> &'g [String] {
    &g.nodes
        .iter()
        .find(|n| n.name == service)
        .expect("service node")
        .calls
}

fn unread(g: &LocalGraph) -> usize {
    g.blind_spots
        .iter()
        .filter(|b| b.kind == "compose-unread-shape")
        .count()
}

#[test]
fn test_build_scalar_is_imported_unchanged() {
    let g = parse("services:\n  web:\n    build: ./app\n");
    assert_eq!(build_sources(&g), vec!["./app"]);
    assert_eq!(unread(&g), 0);
}

#[test]
fn test_build_mapping_imports_context_only() {
    let g = parse(
        "services:\n  web:\n    build:\n      context: ./app\n      dockerfile: Dockerfile.dev\n",
    );
    assert_eq!(build_sources(&g), vec!["./app"]);
    assert_eq!(unread(&g), 0);
}

#[test]
fn test_build_mapping_without_context_is_blind_spot_not_raw_text_import() {
    let g = parse("services:\n  web:\n    build:\n      dockerfile: Dockerfile.dev\n");
    assert!(build_sources(&g).is_empty(), "{:?}", g.imports);
    assert_eq!(unread(&g), 1);
}

#[test]
fn test_build_sequence_is_blind_spot_not_raw_text_import() {
    let g = parse("services:\n  web:\n    build:\n      - ./a\n      - ./b\n");
    assert!(build_sources(&g).is_empty(), "{:?}", g.imports);
    assert_eq!(unread(&g), 1);
}

#[test]
fn test_depends_on_list_form_still_reads_services() {
    let g = parse("services:\n  web:\n    depends_on:\n      - db\n      - cache\n");
    assert_eq!(calls_of(&g, "web"), ["db", "cache"]);
    assert_eq!(unread(&g), 0);
}

#[test]
fn test_depends_on_mapping_form_reads_service_names() {
    let g = parse(
        "services:\n  web:\n    depends_on:\n      db:\n        condition: service_healthy\n      cache:\n        condition: service_started\n",
    );
    assert_eq!(calls_of(&g, "web"), ["db", "cache"]);
    assert_eq!(unread(&g), 0);
}

#[test]
fn test_depends_on_flow_sequence_is_blind_spot() {
    let g = parse("services:\n  web:\n    depends_on: [db, cache]\n");
    assert!(calls_of(&g, "web").is_empty());
    assert_eq!(unread(&g), 1);
}

#[test]
fn test_depends_on_empty_flow_sequence_is_not_blind_spot() {
    let g = parse("services:\n  web:\n    depends_on: []\n");
    assert_eq!(unread(&g), 0);
}

#[test]
fn test_environment_list_form_is_blind_spot() {
    let g = parse("services:\n  web:\n    environment:\n      - KEY=value\n");
    assert!(g.nodes.iter().all(|n| n.name != "KEY"));
    assert_eq!(unread(&g), 1);
}

#[test]
fn test_environment_mapping_form_still_emits_consts_without_blind_spot() {
    let g = parse("services:\n  web:\n    environment:\n      KEY: value\n");
    assert!(g.nodes.iter().any(|n| n.name == "KEY"));
    assert_eq!(unread(&g), 0);
}

#[test]
fn test_empty_and_whitespace_only_compose_yield_no_nodes_or_blind_spots() {
    for src in ["", "   \n"] {
        let g = parse(src);
        assert!(g.nodes.is_empty() && g.imports.is_empty());
        assert_eq!(unread(&g), 0);
    }
}
