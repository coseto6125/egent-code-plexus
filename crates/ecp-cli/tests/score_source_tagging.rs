//! `Hit.score_source` must reflect which ranker produced the score:
//! substring (no tantivy index on disk) vs BM25 (tantivy built).

use ecp_cli::commands::find::{compute_hits, FindArgs, FindMode, ScoreSource};
use ecp_cli::engine::Engine;
use ecp_cli::search::TantivyEngine;
use ecp_core::graph::ZeroCopyGraph;
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

fn make_graph(names: &[&str]) -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    for (i, name) in names.iter().enumerate() {
        let id = fx.func("src/lib.rs", name);
        fx.span(id, (i as u32, 0, i as u32 + 1, 0));
    }
    fx.build()
}

fn persist(dir: &std::path::Path, graph: &ZeroCopyGraph) {
    fs::create_dir_all(dir).unwrap();
    let bytes = rkyv::to_bytes::<Error>(graph).unwrap();
    fs::write(dir.join("graph.bin"), bytes.as_slice()).unwrap();
}

#[test]
fn substring_path_emits_substring_source_tag() {
    let dir = tempdir().unwrap();
    persist(dir.path(), &make_graph(&["parseConfig", "configLoad"]));
    let engine = Engine::load(dir.path().join("graph.bin")).unwrap();

    // No tantivy index on disk → bm25 path falls through to substring_hits.
    let args = FindArgs {
        pattern: Some("config".into()),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        file: None,
        repo: None,
        format: None,
        batch: false,
    };
    let hits = compute_hits(args, &engine).unwrap();
    assert!(!hits.is_empty(), "expected substring hits");
    assert!(
        hits.iter()
            .all(|h| h.score_source == ScoreSource::Substring),
        "expected all hits tagged Substring, got: {:?}",
        hits.iter().map(|h| h.score_source).collect::<Vec<_>>()
    );
}

#[test]
fn tantivy_path_emits_bm25_source_tag() {
    let dir = tempdir().unwrap();
    let graph = make_graph(&["parseConfig", "configLoad"]);
    persist(dir.path(), &graph);
    TantivyEngine::build_index(dir.path(), &graph).unwrap();
    let engine = Engine::load(dir.path().join("graph.bin")).unwrap();

    let args = FindArgs {
        pattern: Some("config".into()),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        file: None,
        repo: None,
        format: None,
        batch: false,
    };
    let hits = compute_hits(args, &engine).unwrap();
    assert!(!hits.is_empty(), "expected tantivy hits");
    assert!(
        hits.iter().all(|h| h.score_source == ScoreSource::Bm25),
        "expected all hits tagged Bm25, got: {:?}",
        hits.iter().map(|h| h.score_source).collect::<Vec<_>>()
    );
}
