//! Regression: `auto_ensure::embeddings_present` must correctly drive
//! the reindex spawn decision so a `git commit` doesn't silently
//! demote a vector-capable graph to BM25-only on the next query.

use graph_nexus_cli::auto_ensure::embeddings_present;
use graph_nexus_core::graph::{
    File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

fn make_graph(embeddings: Option<Vec<Vec<f32>>>) -> ZeroCopyGraph {
    let mut pool = StringPool::new();
    let file_ref = pool.add("src/lib.rs");
    let name_ref = pool.add("validateUser");
    let uid_ref = pool.add("Function:src/lib.rs:validateUser");
    let nodes = vec![Node {
        uid: uid_ref,
        name: name_ref,
        file_idx: 0,
        kind: NodeKind::Function,
        span: (0, 0, 1, 0),
        community_id: 0,
    }];
    ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: file_ref,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Source,
        }],
        nodes,
        edges: vec![],
        out_offsets: vec![0, 0],
        in_offsets: vec![0, 0],
        in_edge_idx: vec![],
        name_index: vec![],
        embeddings,
        process_start: 1,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    }
}

fn write_to_tmp(graph: ZeroCopyGraph) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let graph_path = dir.path().join("graph.bin");
    let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();
    fs::write(&graph_path, bytes).unwrap();
    (dir, graph_path)
}

#[test]
fn embeddings_present_returns_true_when_graph_has_embeddings() {
    let (_dir, path) = write_to_tmp(make_graph(Some(vec![vec![0.1; 1024]])));
    assert!(embeddings_present(&path));
}

#[test]
fn embeddings_present_returns_false_when_graph_has_no_embeddings() {
    let (_dir, path) = write_to_tmp(make_graph(None));
    assert!(!embeddings_present(&path));
}

#[test]
fn embeddings_present_returns_false_for_missing_file() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("nonexistent.bin");
    assert!(!embeddings_present(&missing));
}
