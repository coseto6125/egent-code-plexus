//! `Hit.score_source` must reflect which ranker produced the score:
//! substring (no tantivy index on disk) vs BM25 (tantivy built).

use ecp_cli::commands::find::{compute_hits, FindArgs, FindMode, ScoreSource};
use ecp_cli::engine::Engine;
use ecp_cli::search::TantivyEngine;
use ecp_core::graph::{
    File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

fn make_graph(names: &[&str]) -> ZeroCopyGraph {
    let mut pool = StringPool::new();
    let file_ref = pool.add("src/lib.rs");
    let nodes: Vec<Node> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let name_ref = pool.add(name);
            Node {
                uid: ecp_core::uid::compute(NodeKind::Function, "src/lib.rs", None, name),
                name: name_ref,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (i as u32, 0, i as u32 + 1, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            }
        })
        .collect();
    let n = nodes.len();
    let out_offsets: Vec<u32> = (0..=n as u32).map(|_| 0).collect();
    let in_offsets: Vec<u32> = (0..=n as u32).map(|_| 0).collect();
    ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: file_ref,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        }],
        nodes,
        edges: vec![],
        out_offsets,
        in_offsets,
        in_edge_idx: vec![],
        name_index: Vec::new(),
        process_start: n as u32,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
    }
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
