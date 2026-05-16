//! Integration tests for `gnx shape_check`.
//!
//! Each test hand-rolls a minimal `ZeroCopyGraph` (one consumer node,
//! one route node, one Fetches edge) so the drift logic is exercised
//! without any analyzer / extractor in the loop. The graph is written
//! to a tempdir's `graph.bin`, then invoked via the compiled `gnx`
//! binary with `--graph <path>` so we test the full CLI wire-up,
//! clap parsing, and emit() output path.

use graph_nexus_analyzer::fetch_shape::format_reason;
use graph_nexus_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, RouteShape, ZeroCopyGraph,
    GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

/// Construct a 2-node graph: a consumer Function and a Route. Edges
/// supplied by the caller (so each test can vary the rel_type / reason).
/// `route_shape_keys` decides what the route advertises; `None` means
/// no shape extracted (skipped in shape_check).
fn build_graph(
    edges_spec: &[(u32, u32, RelType, &str)],
    route_shape_keys: Option<(Vec<&str>, Vec<&str>)>,
) -> Vec<u8> {
    let mut pool = StringPool::new();
    let file_ref = pool.add("src/consumer.ts");
    let route_file_ref = pool.add("src/api.ts");
    let consumer_uid = pool.add("Function:src/consumer.ts:fetchUser");
    let consumer_name = pool.add("fetchUser");
    let route_uid = pool.add("Route:src/api.ts:GET /users/:id");
    let route_name = pool.add("GET /users/:id");

    // Pre-intern edge reasons so each Edge.reason can resolve out of
    // the same pool. We collect (offset,len) for the actual Edge build
    // below — clap routes them through StrRef.
    let edge_reasons: Vec<_> = edges_spec.iter().map(|(_, _, _, r)| pool.add(r)).collect();

    // Optionally build a RouteShape entry pointing at node_idx=1 (the Route).
    let route_shapes = match route_shape_keys {
        Some((resp, err)) => {
            let resp_refs: Vec<_> = resp.iter().map(|k| pool.add(k)).collect();
            let err_refs: Vec<_> = err.iter().map(|k| pool.add(k)).collect();
            vec![RouteShape {
                node_idx: 1,
                response_keys: resp_refs,
                error_keys: err_refs,
            }]
        }
        None => vec![],
    };

    let edges: Vec<Edge> = edges_spec
        .iter()
        .zip(edge_reasons.iter())
        .map(|((src, tgt, rel, _), reason_ref)| Edge {
            source: *src,
            target: *tgt,
            rel_type: *rel,
            confidence: 1.0,
            reason: *reason_ref,
        })
        .collect();

    // CSR offsets: 2 nodes → out_offsets/in_offsets have length 3.
    // Build out_offsets by counting outgoing edges per source.
    let n_nodes = 2usize;
    let mut out_counts = vec![0u32; n_nodes];
    let mut in_counts = vec![0u32; n_nodes];
    for e in &edges {
        out_counts[e.source as usize] += 1;
        in_counts[e.target as usize] += 1;
    }
    let mut out_offsets = vec![0u32; n_nodes + 1];
    for i in 0..n_nodes {
        out_offsets[i + 1] = out_offsets[i] + out_counts[i];
    }
    // Edges are stored sorted by source (so out_offsets is a direct slice);
    // the test only supplies pre-sorted edges so this matches the build.
    let mut in_offsets = vec![0u32; n_nodes + 1];
    for i in 0..n_nodes {
        in_offsets[i + 1] = in_offsets[i] + in_counts[i];
    }
    // in_edge_idx maps each incoming slot to the edge index. Build by
    // walking edges and bucketing by target.
    let mut in_edge_idx = vec![0u32; edges.len()];
    let mut cursor = in_offsets.clone();
    for (eidx, e) in edges.iter().enumerate() {
        let t = e.target as usize;
        in_edge_idx[cursor[t] as usize] = eidx as u32;
        cursor[t] += 1;
    }

    let g = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![
            File {
                path: file_ref,
                mtime: 0,
                content_hash: [0; 32],
                category: FileCategory::Source,
            },
            File {
                path: route_file_ref,
                mtime: 0,
                content_hash: [0; 32],
                category: FileCategory::Source,
            },
        ],
        nodes: vec![
            Node {
                uid: consumer_uid,
                name: consumer_name,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (1, 0, 5, 0),
                community_id: 0,
            },
            Node {
                uid: route_uid,
                name: route_name,
                file_idx: 1,
                kind: NodeKind::Route,
                span: (1, 0, 5, 0),
                community_id: 0,
            },
        ],
        edges,
        out_offsets,
        in_offsets,
        in_edge_idx,
        name_index: vec![],
        process_start: 2,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes,
    };

    rkyv::to_bytes::<Error>(&g).unwrap().to_vec()
}

fn write_graph(dir: &Path, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join("graph.bin");
    std::fs::write(&p, bytes).unwrap();
    p
}

fn run_shape_check(graph_path: &Path, format: &str) -> (String, String, bool) {
    let out = Command::new(gnx_bin())
        .args([
            "--graph",
            graph_path.to_str().unwrap(),
            "shape-check",
            "--format",
            format,
        ])
        .output()
        .expect("gnx spawn failed");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn shape_check_reports_no_drift_when_keys_match() {
    // Consumer reads ["id", "name"]; route emits response=["id","name"], error=["msg"].
    // Every consumer key is known → expect "0 drift detected".
    let dir = tempdir().unwrap();
    let reason = format_reason(&["id".to_string(), "name".to_string()], 1);
    let bytes = build_graph(
        &[(0, 1, RelType::Fetches, &reason)],
        Some((vec!["id", "name"], vec!["msg"])),
    );
    let path = write_graph(dir.path(), &bytes);

    let (stdout, stderr, ok) = run_shape_check(&path, "text");
    assert!(ok, "command failed: stderr={stderr}");
    assert!(
        stdout.contains("1 Fetches edge(s), 0 drift detected"),
        "expected zero-drift header, got: {stdout}"
    );
    assert!(
        !stdout.contains("DRIFT"),
        "no DRIFT rows expected, got: {stdout}"
    );
}

#[test]
fn shape_check_flags_unknown_consumer_key() {
    // Consumer reads ["id", "ghost"]; route emits response=["id"], error=["msg"].
    // "ghost" is the drift key — must appear in the JSON drift_keys array
    // and in the text DRIFT row.
    let dir = tempdir().unwrap();
    let reason = format_reason(&["id".to_string(), "ghost".to_string()], 1);
    let bytes = build_graph(
        &[(0, 1, RelType::Fetches, &reason)],
        Some((vec!["id"], vec!["msg"])),
    );
    let path = write_graph(dir.path(), &bytes);

    // JSON path: assert structure.
    let (stdout, stderr, ok) = run_shape_check(&path, "json");
    assert!(ok, "command failed: stderr={stderr}");
    let json: Value = {
        let s = stdout.trim();
        let start = s
            .find('{')
            .unwrap_or_else(|| panic!("non-JSON stdout: {s}"));
        serde_json::from_str(&s[start..])
            .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={s}"))
    };
    assert_eq!(json["status"], "success");
    assert_eq!(json["total_fetches"].as_u64().unwrap(), 1);
    assert_eq!(json["drift_count"].as_u64().unwrap(), 1);
    let drift_arr = json["drift"].as_array().expect("drift array");
    assert_eq!(drift_arr.len(), 1);
    let drift_keys: Vec<&str> = drift_arr[0]["drift_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(drift_keys, vec!["ghost"]);

    // Text path: assert DRIFT row is rendered.
    let (stdout_text, _, ok) = run_shape_check(&path, "text");
    assert!(ok);
    assert!(
        stdout_text.contains("DRIFT") && stdout_text.contains("ghost"),
        "expected DRIFT row mentioning 'ghost', got: {stdout_text}"
    );
    assert!(
        stdout_text.contains("1 Fetches edge(s), 1 with drift"),
        "expected drift summary header, got: {stdout_text}"
    );
}

#[test]
fn shape_check_handles_graph_with_no_fetches_edges() {
    // Graph contains a single Calls edge (no Fetches). The total Fetches
    // count must be 0 and the zero-drift summary must still render.
    let dir = tempdir().unwrap();
    let bytes = build_graph(&[(0, 1, RelType::Calls, "calls")], None);
    let path = write_graph(dir.path(), &bytes);

    let (stdout, stderr, ok) = run_shape_check(&path, "text");
    assert!(ok, "command failed: stderr={stderr}");
    assert!(
        stdout.contains("0 Fetches edge(s), 0 drift detected"),
        "expected empty-fetch summary, got: {stdout}"
    );
}
