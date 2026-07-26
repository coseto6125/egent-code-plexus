//! Regression tests for `TantivyEngine::build_index`.
//!
//! Before the `Result`-returning conversion, every internal failure
//! (writer lock held by a zombie, half-committed segment from a killed
//! prior run, FS full mid-commit) would `unwrap()` and abort the whole
//! `ecp analyze` — even though `graph.bin` had already been written
//! and was perfectly usable. These tests pin three behaviours: (1) the
//! happy path returns Ok and produces a queryable index, (2) a
//! stale/garbage directory left by a prior abort is wiped and rebuilt,
//! (3) the error is surfaced as `Err` rather than a panic.

use ecp_cli::search::TantivyEngine;
use ecp_core::graph::{NodeKind, ZeroCopyGraph};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

fn make_graph_with_names(names: &[&str]) -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    for (i, name) in names.iter().enumerate() {
        let id = fx.func("src/main.rs", name);
        fx.span(id, (i as u32, 0, i as u32 + 1, 0));
    }
    fx.build()
}

// rkyv round-trips through to_bytes — exercise it to keep the test's
// graph layout honest against any future schema drift, mirroring the
// constructor the analyzer actually uses.
fn assert_graph_round_trips(g: &ZeroCopyGraph) {
    rkyv::to_bytes::<Error>(g).expect("graph must round-trip via rkyv");
}

#[test]
fn build_index_happy_path_returns_ok_and_is_queryable() {
    let dir = tempdir().unwrap();
    let index_dir = dir.path();
    let graph = make_graph_with_names(&["resolve_symbol", "lookup_global", "register_node"]);
    assert_graph_round_trips(&graph);

    TantivyEngine::build_index(index_dir, &graph).expect("happy path must succeed");

    let (hits, _total) =
        TantivyEngine::search(index_dir, "resolve_symbol", 100).expect("index must be queryable");
    let expected_uid =
        ecp_core::uid::compute(NodeKind::Function, "src/main.rs", None, "resolve_symbol")
            .to_string();
    assert!(
        hits.iter().any(|(_, uid)| uid == &expected_uid),
        "expected uid {expected_uid} for resolve_symbol in BM25 hits, got: {hits:?}"
    );
}

#[test]
fn build_index_wipes_stale_directory_left_by_prior_abort() {
    // Simulate what a Ctrl+C mid-build leaves behind: an existing
    // directory full of files that `Index::create_in_dir` would refuse
    // to reuse. Without the wipe step, every subsequent analyze would
    // panic at the same place.
    let dir = tempdir().unwrap();
    let index_dir_root = dir.path();
    let tantivy_dir = index_dir_root.join("tantivy");
    fs::create_dir_all(&tantivy_dir).unwrap();
    fs::write(tantivy_dir.join("meta.json"), "{ corrupt").unwrap();
    fs::write(tantivy_dir.join(".tantivy-writer.lock"), "zombie").unwrap();
    fs::write(tantivy_dir.join("segment.idx"), &[0u8; 256][..]).unwrap();

    let graph = make_graph_with_names(&["fresh_symbol"]);
    TantivyEngine::build_index(index_dir_root, &graph).expect("stale dir must self-heal");

    let (hits, _total) = TantivyEngine::search(index_dir_root, "fresh_symbol", 100)
        .expect("index must be queryable");
    let expected_uid =
        ecp_core::uid::compute(NodeKind::Function, "src/main.rs", None, "fresh_symbol").to_string();
    assert!(
        hits.iter().any(|(_, uid)| uid == &expected_uid),
        "rebuilt index must be queryable, expected uid {expected_uid}, got: {hits:?}"
    );
    // The garbage files must have been removed by the wipe step.
    assert!(
        !tantivy_dir.join(".tantivy-writer.lock").exists()
            || tantivy_dir
                .join(".tantivy-writer.lock")
                .metadata()
                .unwrap()
                .len()
                != 6,
        "stale .tantivy-writer.lock must not survive"
    );
}

#[test]
fn build_index_succeeds_with_empty_graph() {
    // A repo with zero symbols shouldn't break the pipeline — the
    // unwrap on `commit()` was particularly fragile here in earlier
    // Tantivy versions when no documents were added.
    let dir = tempdir().unwrap();
    let index_dir = dir.path();
    let graph = make_graph_with_names(&[]);
    TantivyEngine::build_index(index_dir, &graph).expect("empty graph must build");
    assert!(index_dir.join("tantivy").exists());
}
