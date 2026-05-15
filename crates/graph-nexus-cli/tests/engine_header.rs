//! Regression tests for `Engine::load` header validation.
//!
//! Without these guards, a `graph.bin` with corrupted magic bytes or a
//! mismatched on-disk format version would pass `rkyv::access`'s
//! structural check and segfault (or silently misinterpret data) the
//! moment a field is dereferenced.

use graph_nexus_cli::engine::Engine;
use graph_nexus_core::graph::{
    File, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use rkyv::rancor::Error;
use tempfile::tempdir;

fn make_graph(magic: [u8; 8], version: u32) -> Vec<u8> {
    let mut pool = StringPool::new();
    let name_ref = pool.add("entry");
    let uid_ref = pool.add("Function:src/main.ts:entry");
    let g = ZeroCopyGraph {
        magic,
        version,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: name_ref,
            mtime: 0,
            content_hash: [0; 32],
            category: graph_nexus_core::graph::FileCategory::Source,
        }],
        nodes: vec![Node {
            uid: uid_ref,
            name: name_ref,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (1, 0, 5, 0),
            community_id: 0,
        }],
        edges: vec![],
        out_offsets: vec![0, 0],
        in_offsets: vec![0, 0],
        in_edge_idx: vec![],
        name_index: vec![],
        embeddings: None,
        process_start: 1,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    };
    rkyv::to_bytes::<Error>(&g).unwrap().to_vec()
}

#[test]
fn engine_load_accepts_well_formed_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, make_graph(GRAPH_MAGIC, GRAPH_FORMAT_VERSION)).unwrap();
    Engine::load(&path).expect("well-formed header must load");
}

fn expect_invalid_data(path: &std::path::Path, expected_fragment: &str) {
    let Err(err) = Engine::load(path) else {
        panic!("expected InvalidData, got Ok");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    assert!(
        err.to_string().contains(expected_fragment),
        "error message should contain {expected_fragment:?}: {err}"
    );
}

#[test]
fn engine_load_rejects_wrong_magic() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, make_graph(*b"XXXXXXXX", GRAPH_FORMAT_VERSION)).unwrap();
    expect_invalid_data(&path, "bad magic");
}

#[test]
fn engine_load_rejects_unknown_version() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(
        &path,
        make_graph(GRAPH_MAGIC, GRAPH_FORMAT_VERSION.wrapping_add(99)),
    )
    .unwrap();
    expect_invalid_data(&path, "incompatible format version");
}

#[test]
fn engine_load_rejects_garbage_bytes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, b"not an rkyv archive").unwrap();
    let Err(err) = Engine::load(&path) else {
        panic!("garbage bytes must be rejected");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
}
