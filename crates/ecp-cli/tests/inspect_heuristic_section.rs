//! Integration tests for T-H3: heuristic edge split in `ecp inspect`.
//!
//! Each test injects a synthetic graph.bin (overwriting the one produced by
//! `admin index`) to control the exact edge mix. Tests verify:
//!   - deterministic edges stay in `outgoing` / `incoming`
//!   - heuristic edges land in `heuristic_outgoing` / `heuristic_incoming`
//!   - `heuristic_note` appears iff at least one heuristic edge is present
//!   - per-candidate `checks` placeholder is present for every heuristic entry

use ecp_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use ecp_core::pool::StringPool;
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

// ── Shared graph builder ──────────────────────────────────────────────────

struct GraphSpec {
    edges: Vec<(u32, u32, RelType, &'static str)>, // (src, tgt, rel, reason)
}

/// Build a synthetic graph with two Function nodes:
///   node 0 = "alpha"  (src/a.ts)
///   node 1 = "beta"   (src/b.ts)
///
/// `spec.edges` drives the edge set; source/target are indices into the
/// two-node list above.
fn build_graph_bytes(spec: &GraphSpec) -> Vec<u8> {
    let mut pool = StringPool::new();

    let file_a = pool.add("src/a.ts");
    let file_b = pool.add("src/b.ts");
    let uid_alpha = pool.add("Function:src/a.ts:alpha");
    let uid_beta = pool.add("Function:src/b.ts:beta");
    let name_alpha = pool.add("alpha");
    let name_beta = pool.add("beta");

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
            uid: uid_alpha,
            name: name_alpha,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (1, 0, 3, 0),
            community_id: 0,
        },
        Node {
            uid: uid_beta,
            name: name_beta,
            file_idx: 1,
            kind: NodeKind::Function,
            span: (1, 0, 3, 0),
            community_id: 0,
        },
    ];

    let mut edges = Vec::new();
    // out_offsets and in_offsets are rebuilt per edge set.
    let mut out_counts = [0u32; 2];
    let mut in_counts = [0u32; 2];

    for &(src, tgt, rel, reason) in &spec.edges {
        let reason_ref = pool.add(reason);
        edges.push(Edge {
            source: src,
            target: tgt,
            rel_type: rel,
            confidence: 0.8,
            reason: reason_ref,
        });
        out_counts[src as usize] += 1;
        in_counts[tgt as usize] += 1;
    }

    // Build CSR offset arrays (nodes sorted by source for out_offsets,
    // by target for in_offsets).
    let n = nodes.len();
    let mut out_offsets = vec![0u32; n + 1];
    let mut in_offsets = vec![0u32; n + 1];
    for i in 0..n {
        out_offsets[i + 1] = out_offsets[i] + out_counts[i];
        in_offsets[i + 1] = in_offsets[i] + in_counts[i];
    }

    // in_edge_idx: for each node, list edge indices where that node is target.
    let mut in_edge_idx: Vec<u32> = Vec::new();
    for tgt_node in 0..n {
        for (idx, &(_, tgt, _, _)) in spec.edges.iter().enumerate() {
            if tgt as usize == tgt_node {
                in_edge_idx.push(idx as u32);
            }
        }
    }

    let name_index: Vec<u32> = (0..n as u32).collect();

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
        process_start: n as u32,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    };

    rkyv::to_bytes::<Error>(&graph)
        .expect("serialize synthetic graph")
        .into_vec()
}

// ── Harness helpers ───────────────────────────────────────────────────────

fn find_graph_bin(repo: &Path) -> std::path::PathBuf {
    // Layout: .ecp/<repo_slug>/commits/<branch_sha>/graph.bin
    fn walk(dir: &Path) -> Option<std::path::PathBuf> {
        let rd = std::fs::read_dir(dir).ok()?;
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.file_name().map(|n| n == "graph.bin").unwrap_or(false) {
                return Some(p);
            }
            if p.is_dir() {
                if let Some(found) = walk(&p) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(&repo.join(".ecp")).expect("graph.bin not found under .ecp")
}

fn init_repo_and_index(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.ts"), "export function alpha(): void {}\n").unwrap();
    std::fs::write(repo.join("src/b.ts"), "export function beta(): void {}\n").unwrap();

    let run_git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
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
        "admin index: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_inspect_json(repo: &Path, symbol: &str) -> Value {
    let out = Command::new(ecp_bin())
        .args(["inspect", "--name", symbol, "--format", "json"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("inspect spawn failed");
    assert!(
        out.status.success(),
        "inspect: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON:\n{stdout}"));
    serde_json::from_str(&stdout[start..]).unwrap_or_else(|e| panic!("JSON parse: {e}\n{stdout}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// A deterministic `Calls` edge must appear in `outgoing`, not in
/// `heuristic_outgoing`.
#[test]
fn test_deterministic_edges_in_main_section() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    // alpha --Calls--> beta  (deterministic)
    // alpha --MirrorsField--> beta  (heuristic)
    let bytes = build_graph_bytes(&GraphSpec {
        edges: vec![
            (0, 1, RelType::Calls, "direct-call"),
            (0, 1, RelType::MirrorsField, "schema-mirror"),
        ],
    });
    std::fs::write(find_graph_bin(tmp.path()), &bytes).unwrap();

    let out = run_inspect_json(tmp.path(), "alpha");
    assert_eq!(out["status"].as_str(), Some("found"));

    let outgoing = &out["outgoing"];
    assert!(
        outgoing.get("calls").is_some(),
        "Calls edge must appear in outgoing, got {outgoing}"
    );
    assert!(
        outgoing.get("mirrors_field").is_none(),
        "MirrorsField must NOT appear in outgoing, got {outgoing}"
    );
}

/// A `MirrorsField` edge must appear in `heuristic_outgoing`, not in
/// `outgoing`.
#[test]
fn test_heuristic_edges_in_separate_section() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph_bytes(&GraphSpec {
        edges: vec![
            (0, 1, RelType::Calls, "direct-call"),
            (0, 1, RelType::MirrorsField, "schema-mirror"),
        ],
    });
    std::fs::write(find_graph_bin(tmp.path()), &bytes).unwrap();

    let out = run_inspect_json(tmp.path(), "alpha");

    let h_out = &out["heuristic_outgoing"];
    assert!(
        h_out.get("mirrors_field").is_some(),
        "MirrorsField must appear in heuristic_outgoing, got {h_out}"
    );
    assert!(
        h_out.get("calls").is_none(),
        "Calls must NOT appear in heuristic_outgoing, got {h_out}"
    );
}

/// When at least one heuristic edge is present, the top-level
/// `heuristic_note` must equal the spec-mandated string.
#[test]
fn test_heuristic_note_present_when_non_empty() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph_bytes(&GraphSpec {
        edges: vec![(0, 1, RelType::EventTopicMirror, "event-mirror")],
    });
    std::fs::write(find_graph_bin(tmp.path()), &bytes).unwrap();

    let out = run_inspect_json(tmp.path(), "alpha");

    assert_eq!(
        out["heuristic_note"].as_str(),
        Some("verify before acting — candidate edges, may have false positives"),
        "heuristic_note mismatch: {out}"
    );
}

/// When there are NO heuristic edges, `heuristic_note`, `heuristic_outgoing`,
/// and `heuristic_incoming` must be absent from the payload.
#[test]
fn test_heuristic_note_absent_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph_bytes(&GraphSpec {
        edges: vec![(0, 1, RelType::Calls, "direct-call")],
    });
    std::fs::write(find_graph_bin(tmp.path()), &bytes).unwrap();

    let out = run_inspect_json(tmp.path(), "alpha");

    assert!(
        out.get("heuristic_note").is_none(),
        "heuristic_note must be absent when no heuristic edges: {out}"
    );
    assert!(
        out.get("heuristic_outgoing").is_none(),
        "heuristic_outgoing must be absent when empty: {out}"
    );
    assert!(
        out.get("heuristic_incoming").is_none(),
        "heuristic_incoming must be absent when empty: {out}"
    );
}

/// Every entry under a heuristic edge bucket must carry the T-H3 stub
/// placeholder `[UNKNOWN_TIER] checks: <none recorded yet>`.
#[test]
fn test_check_breakdown_visible_per_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph_bytes(&GraphSpec {
        edges: vec![(0, 1, RelType::MirrorsField, "schema-mirror")],
    });
    std::fs::write(find_graph_bin(tmp.path()), &bytes).unwrap();

    let out = run_inspect_json(tmp.path(), "alpha");

    let candidates = out["heuristic_outgoing"]["mirrors_field"]
        .as_array()
        .unwrap_or_else(|| panic!("heuristic_outgoing.mirrors_field not an array: {out}"));

    for candidate in candidates {
        let checks = candidate["checks"]
            .as_str()
            .unwrap_or_else(|| panic!("candidate missing 'checks' field: {candidate}"));
        assert!(
            checks.contains("[UNKNOWN_TIER]"),
            "checks placeholder must contain [UNKNOWN_TIER]: {checks}"
        );
    }
}

/// Mirror of `test_deterministic_edges_in_main_section` for the incoming
/// direction: `beta` receives a `Calls` from `alpha` (deterministic) and
/// a `MirrorsField` (heuristic). The deterministic one must be in `incoming`,
/// the heuristic one in `heuristic_incoming`.
#[test]
fn test_incoming_split_deterministic_vs_heuristic() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph_bytes(&GraphSpec {
        edges: vec![
            (0, 1, RelType::Calls, "direct-call"),
            (0, 1, RelType::MirrorsField, "schema-mirror"),
        ],
    });
    std::fs::write(find_graph_bin(tmp.path()), &bytes).unwrap();

    let out = run_inspect_json(tmp.path(), "beta");
    assert_eq!(out["status"].as_str(), Some("found"));

    let incoming = &out["incoming"];
    assert!(
        incoming.get("calls").is_some(),
        "Calls must appear in incoming for beta, got {incoming}"
    );
    assert!(
        incoming.get("mirrors_field").is_none(),
        "MirrorsField must NOT appear in incoming for beta, got {incoming}"
    );

    let h_in = &out["heuristic_incoming"];
    assert!(
        h_in.get("mirrors_field").is_some(),
        "MirrorsField must appear in heuristic_incoming for beta, got {h_in}"
    );
    assert!(
        h_in.get("calls").is_none(),
        "Calls must NOT appear in heuristic_incoming for beta, got {h_in}"
    );
}
