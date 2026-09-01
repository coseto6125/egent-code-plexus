//! A tantivy index whose writer died before `commit()` opens cleanly and
//! matches nothing: `meta.json` says `segments: []`. Seven of the last forty
//! commit dirs in this machine's cache were in that state, and BM25 plus the
//! PreToolUse hook were silent on each of them. Three guards: such an index
//! reads as absent, a build publishes atomically, and a query-triggered
//! build is joined before the process exits.

use ecp_cli::commands::find::{compute_hits, FindArgs, FindMode};
use ecp_cli::engine::Engine;
use ecp_cli::search::TantivyEngine;
use ecp_core::graph::ZeroCopyGraph;
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use std::fs;
use std::path::Path;
use tantivy::schema::{Schema, STORED, STRING, TEXT};
use tempfile::tempdir;

fn make_graph(names: &[&str]) -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    for (i, name) in names.iter().enumerate() {
        let id = fx.func("src/lib.rs", name);
        fx.span(id, (i as u32 + 1, 0, i as u32 + 2, 0));
    }
    fx.build()
}

fn persist(index_dir: &Path, graph: &ZeroCopyGraph) {
    fs::write(
        index_dir.join("graph.bin"),
        rkyv::to_bytes::<Error>(graph).unwrap(),
    )
    .unwrap();
}

/// The on-disk state a killed writer leaves: the schema and `meta.json`
/// exist, no segment was ever committed.
fn write_uncommitted_index(index_dir: &Path) {
    let mut schema_builder = Schema::builder();
    schema_builder.add_text_field("uid", STRING | STORED);
    schema_builder.add_text_field("name", TEXT);
    let dir = index_dir.join("tantivy");
    fs::create_dir_all(&dir).unwrap();
    tantivy::Index::create_in_dir(&dir, schema_builder.build()).unwrap();
}

fn segment_count(index_dir: &Path) -> usize {
    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(index_dir.join("tantivy/meta.json")).unwrap())
            .unwrap();
    meta["segments"].as_array().map_or(0, Vec::len)
}

#[test]
fn test_search_treats_uncommitted_index_as_absent() {
    let dir = tempdir().unwrap();
    write_uncommitted_index(dir.path());
    assert_eq!(
        segment_count(dir.path()),
        0,
        "fixture must have no segments"
    );
    assert!(TantivyEngine::search(dir.path(), "resolve_symbol", 10).is_none());
}

#[test]
fn test_compute_hits_falls_back_to_substring_on_uncommitted_index() {
    let dir = tempdir().unwrap();
    let graph = make_graph(&["resolve_symbol", "lookup_global"]);
    persist(dir.path(), &graph);
    write_uncommitted_index(dir.path());
    let engine = Engine::load(dir.path().join("graph.bin")).unwrap();
    let hits = compute_hits(
        FindArgs {
            pattern: Some("resolve_symbol".into()),
            mode: FindMode::Bm25,
            fuzzy: false,
            all: false,
            include_tests: false,
            kind: None,
            file: None,
            repo: None,
            format: None,
            batch: false,
        },
        &engine,
    )
    .unwrap();
    assert_eq!(hits.len(), 1, "the exact name must still be found");
    assert_eq!(hits[0].name, "resolve_symbol");
}

#[test]
fn test_build_index_publishes_a_committed_index_and_sweeps_leftovers() {
    let dir = tempdir().unwrap();
    let leftover = dir.path().join("tantivy.building.4242");
    fs::create_dir_all(&leftover).unwrap();
    fs::write(leftover.join("meta.json"), "{}").unwrap();
    write_uncommitted_index(dir.path());

    let graph = make_graph(&["resolve_symbol"]);
    TantivyEngine::build_index(dir.path(), &graph).unwrap();

    assert!(
        segment_count(dir.path()) >= 1,
        "published index must be committed"
    );
    assert!(!leftover.exists(), "a killed build's dir is swept");
    let building: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("tantivy.building.")
        })
        .collect();
    assert!(
        building.is_empty(),
        "no build dir survives a successful publish"
    );
    let (hits, _) = TantivyEngine::search(dir.path(), "resolve_symbol", 10).unwrap();
    assert_eq!(hits.len(), 1);
}
