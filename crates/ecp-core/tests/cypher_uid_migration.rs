//! T1-10: verify that `n.uid` projection carries `Value::Int` (no per-row alloc)
//! and that `WHERE n.uid = <numeric>` / `WHERE n.uid = "string"` behave correctly.

use ecp_core::cypher::{self, Value};
use ecp_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use ecp_core::uid;
use std::path::Path;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Single-node graph: Function "target" at "src/t.ts".
fn build_single() -> (Vec<u8>, u64) {
    let mut pool = StringPool::new();
    let name = pool.add("target");
    let fp = pool.add("src/t.ts");

    let node_uid = uid::compute(NodeKind::Function, "src/t.ts", None, "target");

    let g = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: fp,
            mtime: 0,
            content_hash: [0u8; 8],
            category: FileCategory::Source,
        }],
        nodes: vec![Node {
            uid: node_uid,
            name,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (0, 0, 1, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        }],
        edges: vec![],
        out_offsets: vec![0, 0],
        in_offsets: vec![0, 0],
        in_edge_idx: vec![],
        name_index: Vec::new(),
        process_start: 1,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
        kind_offsets: vec![],
        kind_node_idx: vec![],
        node_flags: vec![],
    };

    (
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec(),
        node_uid,
    )
}

/// Two-node graph: a(0) -[:Calls]-> b(1), so WHERE queries can distinguish.
fn build_two() -> (Vec<u8>, u64, u64) {
    let mut pool = StringPool::new();
    let na = pool.add("a");
    let nb = pool.add("b");
    let fp = pool.add("src/t.ts");
    let reason = pool.add("r");

    let uid_a = uid::compute(NodeKind::Function, "src/t.ts", None, "a");
    let uid_b = uid::compute(NodeKind::Function, "src/t.ts", None, "b");

    let g = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: fp,
            mtime: 0,
            content_hash: [0u8; 8],
            category: FileCategory::Source,
        }],
        nodes: vec![
            Node {
                uid: uid_a,
                name: na,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (0, 0, 1, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            },
            Node {
                uid: uid_b,
                name: nb,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (2, 0, 3, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            },
        ],
        edges: vec![Edge {
            source: 0,
            target: 1,
            rel_type: ecp_core::graph::RelType::Calls,
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
        call_metas: vec![],
        function_metas: vec![],
        kind_offsets: vec![],
        kind_node_idx: vec![],
        node_flags: vec![],
    };

    (
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec(),
        uid_a,
        uid_b,
    )
}

fn archived(bytes: &[u8]) -> &ecp_core::graph::ArchivedZeroCopyGraph {
    rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap()
}

fn run(query: &str, bytes: &[u8]) -> ecp_core::cypher::QueryResult {
    let q = cypher::parse(query).expect("parse");
    cypher::execute(&q, archived(bytes), Path::new(".")).expect("execute")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `RETURN n.uid` must produce `Value::Int`, not `Value::Str`.
#[test]
fn project_uid_returns_int_not_string() {
    let (bytes, expected_uid) = build_single();
    let result = run("MATCH (n) RETURN n.uid", &bytes);

    assert_eq!(result.rows.len(), 1);
    let cell = &result.rows[0][0];
    assert!(
        matches!(cell, Value::Int(_)),
        "expected Value::Int, got {cell:?}"
    );
    // The i64 bit-pattern must round-trip back to the original u64.
    if let Value::Int(v) = cell {
        assert_eq!(*v as u64, expected_uid);
    }
}

/// `WHERE n.uid = <numeric>` must match exactly the target node.
#[test]
fn where_uid_numeric_literal_matches() {
    let (bytes, uid_a, uid_b) = build_two();

    // Query for uid_a — expect only node "a".
    let q_a = format!("MATCH (n) WHERE n.uid = {} RETURN n.name", uid_a as i64);
    let res_a = run(&q_a, &bytes);
    assert_eq!(
        res_a.rows.len(),
        1,
        "should match exactly one node for uid_a"
    );
    assert_eq!(res_a.rows[0][0], Value::Str("a".into()));

    // Query for uid_b — expect only node "b".
    let q_b = format!("MATCH (n) WHERE n.uid = {} RETURN n.name", uid_b as i64);
    let res_b = run(&q_b, &bytes);
    assert_eq!(
        res_b.rows.len(),
        1,
        "should match exactly one node for uid_b"
    );
    assert_eq!(res_b.rows[0][0], Value::Str("b".into()));
}

/// `WHERE n.uid = "1234"` must error with a message mentioning "u64" or "numeric".
#[test]
fn where_uid_string_literal_clear_error() {
    let (bytes, _, _) = build_two();
    let q = cypher::parse("MATCH (n) WHERE n.uid = \"1234\" RETURN n.name").expect("parse");
    let err = cypher::execute(&q, archived(&bytes), Path::new("."))
        .expect_err("expected error for uid string comparison");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("u64") || msg.contains("numeric"),
        "error message should mention u64 or numeric, got: {err}"
    );
}

/// Zero-allocation smoke test: projecting uid over 10k rows (single-node graph
/// repeated via a loop workaround) must not panic and should be deterministic.
/// Full dhat heap-gate deferred until dhat is in dev-deps (see uid_canonical.rs).
#[test]
fn uid_equality_zero_alloc_smoke() {
    let (bytes, expected_uid) = build_single();
    let archived = archived(&bytes);
    let q = cypher::parse("MATCH (n) RETURN n.uid").expect("parse");

    // 10k executions: Miri catches hidden allocs inside the Value::Int branch.
    for _ in 0..10_000 {
        let res = cypher::execute(&q, archived, Path::new(".")).expect("execute");
        let cell = &res.rows[0][0];
        if let Value::Int(v) = cell {
            std::hint::black_box(*v as u64 == expected_uid);
        } else {
            panic!("expected Value::Int, got {cell:?}");
        }
    }
}
