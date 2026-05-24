//! T-H1: heuristic edge filter integration tests.
//!
//! Each test builds a minimal synthetic `ZeroCopyGraph`, injects it as
//! `graph.bin` after `admin index`, then drives `ecp impact` via `Command`.

use ecp_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use rkyv::rancor::Error;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Minimal TypeScript fixture so `admin index` succeeds and creates `.ecp/`.
const SOURCE_A: &str = "export function alpha() { return 1; }\n";
const SOURCE_B: &str =
    "import { alpha } from \"./a\";\nexport function beta() { return alpha(); }\n";

fn init_repo(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.ts"), SOURCE_A).unwrap();
    std::fs::write(repo.join("src/b.ts"), SOURCE_B).unwrap();

    let run_git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(&["init", "-q", "-b", "main"]);
    run_git(&["add", "-A"]);
    run_git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);

    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index spawn failed");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn find_graph_bin(repo: &Path) -> std::path::PathBuf {
    fn walk(dir: &Path, depth: usize) -> Option<std::path::PathBuf> {
        if depth == 0 {
            return None;
        }
        let rd = std::fs::read_dir(dir).ok()?;
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.file_name().map(|n| n == "graph.bin").unwrap_or(false) {
                return Some(p);
            }
            if p.is_dir() {
                if let Some(found) = walk(&p, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(&repo.join(".ecp"), 5).expect("graph.bin not found after admin index")
}

/// Two `Function` nodes: `source` (idx 0) and `target` (idx 1), linked by a
/// single directed edge of the given `rel_type` (source → target).
///
/// CSR layout:
/// - out_offsets: source has 1 outgoing edge, target has 0.
/// - in_offsets: target has 1 incoming edge (edge 0), source has 0.
fn synthetic_graph_two_nodes(rel_type: RelType, reason_str: &str) -> Vec<u8> {
    let mut pool = StringPool::new();
    let file_a = pool.add("src/a.ts");
    let file_b = pool.add("src/b.ts");
    let src_uid = ecp_core::uid::compute(
        ecp_core::graph::NodeKind::Function,
        "src/a.ts",
        None,
        "source",
    );
    let tgt_uid = ecp_core::uid::compute(
        ecp_core::graph::NodeKind::Function,
        "src/b.ts",
        None,
        "target",
    );
    let src_name = pool.add("source");
    let tgt_name = pool.add("target");
    let reason_ref = pool.add(reason_str);

    let files = vec![
        File {
            path: file_a,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
        File {
            path: file_b,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
    ];
    let nodes = vec![
        Node {
            uid: src_uid,
            name: src_name,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (1, 0, 3, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        },
        Node {
            uid: tgt_uid,
            name: tgt_name,
            file_idx: 1,
            kind: NodeKind::Function,
            span: (2, 0, 4, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        },
    ];
    // source (0) → target (1)
    let edges = vec![Edge {
        source: 0,
        target: 1,
        rel_type,
        confidence: 0.6,
        reason: reason_ref,
    }];
    let out_offsets = vec![0u32, 1, 1];
    let in_offsets = vec![0u32, 0, 1];
    let in_edge_idx = vec![0u32];
    let name_index: Vec<ecp_core::graph::NameIndexEntry> = Vec::new();

    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files,
        nodes,
        edges,
        out_offsets,
        in_offsets,
        in_edge_idx,
        name_index,
        process_start: 2,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
        kind_offsets: vec![],
        kind_node_idx: vec![],
        node_flags: vec![],
    };
    rkyv::to_bytes::<Error>(&graph)
        .expect("serialize synthetic graph")
        .into_vec()
}

/// Clean graph: `source` and `target` connected by a deterministic `Calls`
/// edge (no heuristic edges at all).
fn synthetic_graph_clean() -> Vec<u8> {
    synthetic_graph_two_nodes(RelType::Calls, "call")
}

fn run_ecp_impact(repo: &Path, extra_args: &[&str]) -> serde_json::Value {
    let mut cmd_args = vec!["impact", "source", "--format", "json", "--repo", "."];
    cmd_args.extend_from_slice(extra_args);
    let out = Command::new(ecp_bin())
        .args(&cmd_args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp impact failed to spawn");
    assert!(
        out.status.success(),
        "ecp impact exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout:\n{stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"))
}

/// Default `ecp impact` must NOT traverse a `MirrorsField` heuristic edge.
/// `hidden_heuristic_edges: 1` must appear in the output.
#[test]
fn test_default_excludes_heuristic_edges() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(
        &graph_bin,
        synthetic_graph_two_nodes(RelType::MirrorsField, "schema-mirror-heuristic"),
    )
    .unwrap();

    // direction=down so we traverse from source outward.
    let val = run_ecp_impact(tmp.path(), &["--direction", "down"]);

    // impact array must not contain `target` (heuristic edge not traversed).
    if let Some(arr) = val["impact"].as_array() {
        for entry in arr {
            let name = entry["name"].as_str().unwrap_or("");
            assert_ne!(
                name, "target",
                "heuristic target leaked into default impact: {val}"
            );
        }
    }

    // hidden_heuristic_edges must be 1.
    let hidden = val["hidden_heuristic_edges"]
        .as_u64()
        .unwrap_or_else(|| panic!("hidden_heuristic_edges missing from output:\n{val}"));
    assert_eq!(hidden, 1, "expected 1 hidden heuristic edge, got {hidden}");
}

/// With `--include-heuristic`, the BFS traverses the heuristic edge and the
/// reached node appears in `heuristic_edges`, NOT in `impact`.
#[test]
fn test_include_heuristic_flag_traverses() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(
        &graph_bin,
        synthetic_graph_two_nodes(RelType::MirrorsField, "schema-mirror-heuristic"),
    )
    .unwrap();

    let val = run_ecp_impact(tmp.path(), &["--direction", "down", "--include-heuristic"]);

    // `target` must appear in `heuristic_edges`, not in `impact`.
    let in_impact = val["impact"]
        .as_array()
        .map(|arr| arr.iter().any(|e| e["name"].as_str() == Some("target")))
        .unwrap_or(false);
    assert!(
        !in_impact,
        "`target` must not appear in `impact` section: {val}"
    );

    let in_heuristic = val["heuristic_edges"]
        .as_array()
        .map(|arr| arr.iter().any(|e| e["name"].as_str() == Some("target")))
        .unwrap_or(false);
    assert!(
        in_heuristic,
        "`target` must appear in `heuristic_edges` section: {val}"
    );
}

/// Clean graph with no heuristic edges: `hidden_heuristic_edges` is present
/// and equals 0 (noise-reduction parity with `hidden_edges` being omitted when
/// 0, but heuristic count is always written so callers can branch on the field).
#[test]
fn test_zero_heuristic_edges_renders_zero() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(&graph_bin, synthetic_graph_clean()).unwrap();

    let val = run_ecp_impact(tmp.path(), &["--direction", "down"]);

    let hidden = val["hidden_heuristic_edges"]
        .as_u64()
        .unwrap_or_else(|| panic!("hidden_heuristic_edges missing from output:\n{val}"));
    assert_eq!(
        hidden, 0,
        "expected 0 hidden heuristic edges on clean graph, got {hidden}"
    );
}

/// `--explain-confidence` emits the `explain_confidence` block with
/// `threshold: 0.85` and `edges_filtered_by_tier`.
#[test]
fn test_explain_confidence_flag_emits_block() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(
        &graph_bin,
        synthetic_graph_two_nodes(RelType::MirrorsField, "schema-mirror-heuristic"),
    )
    .unwrap();

    let val = run_ecp_impact(tmp.path(), &["--direction", "down", "--explain-confidence"]);

    let ec = &val["explain_confidence"];
    assert!(
        !ec.is_null(),
        "`explain_confidence` block missing from output:\n{val}"
    );
    let threshold = ec["threshold"]
        .as_f64()
        .unwrap_or_else(|| panic!("`explain_confidence.threshold` missing:\n{val}"));
    assert!(
        (threshold - 0.85).abs() < 1e-5,
        "expected threshold 0.85, got {threshold}"
    );
    assert!(
        ec["edges_filtered_by_tier"].is_object(),
        "`edges_filtered_by_tier` must be an object:\n{val}"
    );
}
