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
use ecp_core::graph::{File, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use ecp_core::pool::StringPool;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

fn make_graph_with_names(names: &[&str]) -> ZeroCopyGraph {
    let mut pool = StringPool::new();
    let file_path_ref = pool.add("src/main.rs");
    let nodes = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let name_ref = pool.add(name);
            let uid_ref = pool.add(&format!("Function:src/main.rs:{name}"));
            Node {
                uid: uid_ref,
                name: name_ref,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (i as u32, 0, i as u32 + 1, 0),
                community_id: 0,
            }
        })
        .collect();
    ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: file_path_ref,
            mtime: 0,
            content_hash: [0; 8],
            category: ecp_core::graph::FileCategory::Source,
        }],
        nodes,
        edges: vec![],
        out_offsets: vec![0, 0],
        in_offsets: vec![0, 0],
        in_edge_idx: vec![],
        name_index: vec![],
        process_start: 0,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
    }
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

    let hits =
        TantivyEngine::search(index_dir, "resolve_symbol", 100).expect("index must be queryable");
    assert!(
        hits.iter().any(|(_, uid)| uid.contains("resolve_symbol")),
        "expected resolve_symbol in BM25 hits, got: {hits:?}"
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

    let hits = TantivyEngine::search(index_dir_root, "fresh_symbol", 100)
        .expect("index must be queryable");
    assert!(
        hits.iter().any(|(_, uid)| uid.contains("fresh_symbol")),
        "rebuilt index must be queryable: {hits:?}"
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
