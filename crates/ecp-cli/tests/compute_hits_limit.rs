//! `compute_hits_limited` is `compute_hits` that stops building rows early.
//! Contract: the rows are exactly the head of the unlimited result, same order,
//! same fields, so the PreToolUse hook shows the same block as before while it
//! builds five rows instead of up to a hundred.

use ecp_cli::commands::find::{compute_hits, compute_hits_limited, FindArgs, FindMode, Hit};
use ecp_cli::commands::hook::pre_tool_use::format_hits;
use ecp_cli::engine::Engine;
use ecp_cli::search::TantivyEngine;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

const SYMBOLS: usize = 14;
const LIMIT: usize = 5;

fn make_graph() -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    let ids: Vec<u32> = (0..SYMBOLS)
        .map(|i| {
            let id = fx.func("src/handlers.rs", &format!("handleRequest{i}"));
            fx.span(id, (i as u32, 0, i as u32 + 1, 0));
            id
        })
        .collect();
    for pair in ids.windows(2) {
        fx.edge_with(pair[0], pair[1], RelType::Calls, 1.0, "call");
    }
    fx.build()
}

fn engine_for(dir: &std::path::Path, with_tantivy: bool) -> Engine {
    let graph = make_graph();
    fs::create_dir_all(dir).unwrap();
    let bytes = rkyv::to_bytes::<Error>(&graph).expect("rkyv serialize");
    fs::write(dir.join("graph.bin"), bytes.as_slice()).unwrap();
    if with_tantivy {
        TantivyEngine::build_index(dir, &graph).expect("tantivy build");
    }
    Engine::load(dir.join("graph.bin")).expect("engine load")
}

fn args(pattern: &str) -> FindArgs {
    FindArgs {
        pattern: Some(pattern.to_string()),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        file: None,
        repo: None,
        format: None,
        batch: false,
    }
}

fn rows(hits: &[Hit]) -> Vec<String> {
    hits.iter().map(|h| format!("{h:?}")).collect()
}

fn assert_limited_is_head_of_unlimited(with_tantivy: bool) {
    let dir = tempdir().unwrap();
    let engine = engine_for(dir.path(), with_tantivy);

    let all = compute_hits(args("handle"), &engine).unwrap();
    assert!(
        all.len() > LIMIT,
        "fixture must exceed the limit: {}",
        all.len()
    );
    let limited = compute_hits_limited(args("handle"), &engine, LIMIT).unwrap();

    assert_eq!(limited.len(), LIMIT);
    assert_eq!(rows(&limited), rows(&all[..LIMIT]));
    assert_eq!(format_hits(&limited), format_hits(&all));
}

#[test]
fn test_compute_hits_limited_tantivy_path_returns_head_of_unlimited_rows() {
    assert_limited_is_head_of_unlimited(true);
}

#[test]
fn test_compute_hits_limited_substring_path_returns_head_of_unlimited_rows() {
    assert_limited_is_head_of_unlimited(false);
}

#[test]
fn test_compute_hits_limited_zero_returns_no_rows() {
    let dir = tempdir().unwrap();
    let engine = engine_for(dir.path(), true);
    assert!(compute_hits_limited(args("handle"), &engine, 0)
        .unwrap()
        .is_empty());
}

#[test]
fn test_compute_hits_limited_above_match_count_returns_every_row() {
    let dir = tempdir().unwrap();
    let engine = engine_for(dir.path(), true);
    let all = compute_hits(args("handle"), &engine).unwrap();
    let limited = compute_hits_limited(args("handle"), &engine, 1000).unwrap();
    assert_eq!(rows(&limited), rows(&all));
}
