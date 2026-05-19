//! Smoke test for the promoted `pub fn build_payload` in `commands::shape_check`.
//!
//! Uses the binary integration pattern (Option A): invokes `cgn shape-check`
//! via the compiled binary and asserts the JSON output shape is intact.
//! The fixture writes a minimal `graph.bin` (same helper as `shape_check_cmd.rs`)
//! and confirms `build_payload` returns `{status, total_fetches, drift_count, drift}`.

use cgn_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use cgn_core::pool::StringPool;
use rkyv::rancor::Error;
use serde_json::Value;
use std::process::Command;

mod common;
use common::{cgn_bin, write_graph};

fn build_empty_graph() -> Vec<u8> {
    let pool = StringPool::new();
    let g = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![],
        nodes: vec![],
        edges: vec![],
        out_offsets: vec![0],
        in_offsets: vec![0],
        in_edge_idx: vec![],
        name_index: vec![],
        process_start: 0,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    };
    rkyv::to_bytes::<Error>(&g).unwrap().to_vec()
}

fn build_graph_with_calls_edge() -> Vec<u8> {
    let mut pool = StringPool::new();
    let file_ref = pool.add("src/a.ts");
    let uid_a = pool.add("Function:src/a.ts:foo");
    let name_a = pool.add("foo");
    let uid_b = pool.add("Function:src/a.ts:bar");
    let name_b = pool.add("bar");
    let reason = pool.add("ast-call");

    let g = ZeroCopyGraph {
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
        nodes: vec![
            Node {
                uid: uid_a,
                name: name_a,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (0, 0, 1, 0),
                community_id: 0,
            },
            Node {
                uid: uid_b,
                name: name_b,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (2, 0, 3, 0),
                community_id: 0,
            },
        ],
        edges: vec![Edge {
            source: 0,
            target: 1,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason,
        }],
        out_offsets: vec![0, 1, 1],
        in_offsets: vec![0, 0, 1],
        in_edge_idx: vec![0],
        name_index: vec![],
        process_start: 2,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    };
    rkyv::to_bytes::<Error>(&g).unwrap().to_vec()
}

#[test]
fn shape_check_build_payload_empty_graph_returns_success_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_graph(dir.path(), &build_empty_graph());

    let out = Command::new(cgn_bin())
        .args([
            "--graph",
            path.to_str().unwrap(),
            "shape-check",
            "--format",
            "json",
        ])
        .output()
        .expect("cgn shape-check failed to spawn");

    assert!(
        out.status.success(),
        "shape-check failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("expected JSON on stdout, got: {stdout}"));
    let val: Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"));

    assert_eq!(val["status"], "success", "expected status=success: {val}");
    assert!(
        val.get("total_fetches").is_some(),
        "missing total_fetches: {val}"
    );
    assert!(
        val.get("drift_count").is_some(),
        "missing drift_count: {val}"
    );
    assert!(val.get("drift").is_some(), "missing drift array: {val}");
    assert_eq!(
        val["total_fetches"].as_u64().unwrap(),
        0,
        "empty graph: 0 fetches"
    );
    assert_eq!(
        val["drift_count"].as_u64().unwrap(),
        0,
        "empty graph: 0 drift"
    );
}

#[test]
fn shape_check_build_payload_no_fetches_edge_zero_drift() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_graph(dir.path(), &build_graph_with_calls_edge());

    let out = Command::new(cgn_bin())
        .args([
            "--graph",
            path.to_str().unwrap(),
            "shape-check",
            "--format",
            "json",
        ])
        .output()
        .expect("cgn shape-check failed to spawn");

    assert!(
        out.status.success(),
        "shape-check failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("non-JSON stdout: {stdout}"));
    let val: Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}"));

    assert_eq!(val["status"], "success");
    assert_eq!(val["total_fetches"].as_u64().unwrap(), 0);
    assert_eq!(val["drift_count"].as_u64().unwrap(), 0);
}
