//! Integration tests for `ecp find`.
//!
//! Uses the same minimal-graph fixture pattern as `tests/search_cmd.rs`.
//! 14-language coverage applies to parser / graph primitives — `find` is a
//! CLI-level subcommand, so one focused test suite is sufficient (noted in PR
//! body per CLAUDE.md).

use ecp_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

// ── Fixture helpers ───────────────────────────────────────────────────────────

struct NodeSpec<'a> {
    name: &'a str,
    kind: NodeKind,
    category: FileCategory,
    file: &'a str,
    line: u32,
}

/// Build a minimal graph with the supplied nodes and write `graph.bin`.
/// Edges can be added via `extra_edges` to set caller counts.
fn build_graph(nodes_spec: &[NodeSpec<'_>], extra_edges: &[(usize, usize)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let mut pool = StringPool::new();

    // Collect unique file paths
    let mut file_paths: Vec<&str> = nodes_spec.iter().map(|n| n.file).collect();
    file_paths.dedup();
    // Build file list preserving insertion order
    let mut seen_files: Vec<&str> = Vec::new();
    for n in nodes_spec {
        if !seen_files.contains(&n.file) {
            seen_files.push(n.file);
        }
    }

    let files: Vec<File> = seen_files
        .iter()
        .map(|&path| File {
            path: pool.add(path),
            mtime: 0,
            content_hash: [0; 8],
            category: nodes_spec
                .iter()
                .find(|n| n.file == path)
                .map(|n| n.category)
                .unwrap_or(FileCategory::Source),
        })
        .collect();

    let nodes: Vec<Node> = nodes_spec
        .iter()
        .map(|ns| {
            let file_idx = seen_files.iter().position(|&p| p == ns.file).unwrap() as u32;
            Node {
                uid: ecp_core::uid::compute(ns.kind, ns.file, None, ns.name),
                name: pool.add(ns.name),
                file_idx,
                kind: ns.kind,
                span: (ns.line, 0, ns.line + 10, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            }
        })
        .collect();

    let n = nodes.len();

    // Build edges from caller relationships
    let edges: Vec<Edge> = extra_edges
        .iter()
        .map(|&(src, tgt)| Edge {
            source: src as u32,
            target: tgt as u32,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason: pool.add("test"),
        })
        .collect();

    // out_offsets: no outgoing edges from sources in extra_edges for simplicity
    // Use a flat out_offsets (all zero) and build in_offsets from extra_edges
    let out_offsets = vec![0u32; n + 1];

    // Build in_edge_idx and in_offsets
    let mut incoming_per_node: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (edge_idx, &(_, tgt)) in extra_edges.iter().enumerate() {
        incoming_per_node[tgt].push(edge_idx);
    }
    let mut in_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut in_edge_idx: Vec<u32> = Vec::new();
    in_offsets.push(0);
    for incoming in &incoming_per_node {
        for &eidx in incoming {
            in_edge_idx.push(eidx as u32);
        }
        in_offsets.push(in_edge_idx.len() as u32);
    }

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
        process_start: n as u32,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
    };

    let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();
    let graph_path = dir.path().join("graph.bin");
    std::fs::write(&graph_path, &bytes).unwrap();
    (dir, graph_path)
}

fn run_find(graph: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ecp_bin())
        .arg("find")
        .args(args)
        .arg("--graph")
        .arg(graph)
        .output()
        .expect("ecp find spawn")
}

fn parse_json_stdout(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout: {stdout}"));
    serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|e| panic!("JSON parse error: {e}\nstdout: {stdout}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn find_exact_match_returns_single_definition() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "ensure_index",
            kind: NodeKind::Function,
            category: FileCategory::Source,
            file: "src/auto_ensure.rs",
            line: 27,
        }],
        &[],
    );
    let out = run_find(&graph, &["ensure_index", "--format", "json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], true);
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["name"], "ensure_index");
    assert_eq!(matches[0]["kind"], "Function");
    assert_eq!(matches[0]["file"], "src/auto_ensure.rs");
}

#[test]
fn find_no_match_returns_found_false() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "some_func",
            kind: NodeKind::Function,
            category: FileCategory::Source,
            file: "src/lib.rs",
            line: 1,
        }],
        &[],
    );
    let out = run_find(&graph, &["nonexistent_symbol_xyz", "--format", "json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], false);
    assert_eq!(json["matches"].as_array().unwrap().len(), 0);
    assert_eq!(json["status"], "success");
}

#[test]
fn find_multiple_definitions_returns_top_1_by_default() {
    // Two nodes with same name: source (priority=0) vs test (priority=3).
    // Source should win.
    let (_dir, graph) = build_graph(
        &[
            NodeSpec {
                name: "do_work",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/worker.rs",
                line: 10,
            },
            NodeSpec {
                name: "do_work",
                kind: NodeKind::Function,
                category: FileCategory::Test,
                file: "tests/worker_test.rs",
                line: 5,
            },
        ],
        &[],
    );
    // Without --include-tests, only source variant is reachable.
    let out = run_find(&graph, &["do_work", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], true);
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1, "top-1 by default");
    assert_eq!(matches[0]["category"], "Source");
}

#[test]
fn find_all_returns_all_matches() {
    let (_dir, graph) = build_graph(
        &[
            NodeSpec {
                name: "handle",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/a.rs",
                line: 1,
            },
            NodeSpec {
                name: "handle",
                kind: NodeKind::Method,
                category: FileCategory::Source,
                file: "src/b.rs",
                line: 5,
            },
        ],
        &[],
    );
    let out = run_find(&graph, &["handle", "--all", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2, "--all should return both definitions");
}

#[test]
fn find_fuzzy_skips_test_files_by_default() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "my_func",
            kind: NodeKind::Function,
            category: FileCategory::Test,
            file: "tests/my_test.rs",
            line: 3,
        }],
        &[],
    );
    let out = run_find(&graph, &["my_func", "--mode", "fuzzy", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    // Test file only — should not be returned by default
    assert_eq!(json["found"], false);
}

#[test]
fn find_include_tests_surfaces_test_hits() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "my_func",
            kind: NodeKind::Function,
            category: FileCategory::Test,
            file: "tests/my_test.rs",
            line: 3,
        }],
        &[],
    );
    let out = run_find(
        &graph,
        &[
            "my_func",
            "--mode",
            "fuzzy",
            "--include-tests",
            "--format",
            "json",
        ],
    );
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], true);
    assert_eq!(json["matches"][0]["category"], "Test");
}

#[test]
fn find_fuzzy_substring_match() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "build_query_string",
            kind: NodeKind::Function,
            category: FileCategory::Source,
            file: "src/query.rs",
            line: 20,
        }],
        &[],
    );
    let out = run_find(&graph, &["query_string", "--fuzzy", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], true);
    assert_eq!(json["matches"][0]["name"], "build_query_string");
}

#[test]
fn find_kind_filter_excludes_other_kinds() {
    let (_dir, graph) = build_graph(
        &[
            NodeSpec {
                name: "Config",
                kind: NodeKind::Class,
                category: FileCategory::Source,
                file: "src/config.rs",
                line: 1,
            },
            NodeSpec {
                name: "Config",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/helpers.rs",
                line: 50,
            },
        ],
        &[],
    );
    let out = run_find(
        &graph,
        &["Config", "--kind", "class", "--all", "--format", "json"],
    );
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["kind"], "Class");
}

#[test]
fn find_json_output_shape() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "process",
            kind: NodeKind::Function,
            category: FileCategory::Source,
            file: "src/proc.rs",
            line: 42,
        }],
        &[],
    );
    let out = run_find(&graph, &["process", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    // Top-level shape
    assert!(json.get("found").is_some());
    assert!(json.get("matches").is_some());
    assert!(json.get("status").is_some());
    assert_eq!(json["status"], "success");
    // Match row shape
    let m = &json["matches"][0];
    assert!(m.get("file").is_some());
    assert!(m.get("line").is_some());
    assert!(m.get("name").is_some());
    assert!(m.get("kind").is_some());
    assert!(m.get("category").is_some());
    assert!(m.get("caller_count").is_some());
    assert!(m.get("signature").is_some());
}

#[test]
fn find_toon_output_shape() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "boot",
            kind: NodeKind::Function,
            category: FileCategory::Source,
            file: "src/main.rs",
            line: 1,
        }],
        &[],
    );
    let out = run_find(&graph, &["boot", "--format", "toon"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // TOON output is non-empty and not raw JSON.
    assert!(!stdout.is_empty());
    assert!(!stdout.trim_start().starts_with('{'));
}

#[test]
fn find_ranking_prefers_higher_caller_count_within_same_category() {
    // Two Source nodes: node 0 has 2 callers, node 1 has 0. Node 0 should win.
    let (_dir, graph) = build_graph(
        &[
            NodeSpec {
                name: "init",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/a.rs",
                line: 1,
            },
            NodeSpec {
                name: "caller_a",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/b.rs",
                line: 1,
            },
            NodeSpec {
                name: "caller_b",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/c.rs",
                line: 1,
            },
            NodeSpec {
                name: "init",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/z.rs",
                line: 99,
            },
        ],
        // caller_a (1) → init (0), caller_b (2) → init (0) — node 0 gets 2 callers
        &[(1, 0), (2, 0)],
    );
    let out = run_find(&graph, &["init", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], true);
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    // The node with 2 callers should rank first
    assert_eq!(matches[0]["caller_count"], 2);
    assert_eq!(matches[0]["file"], "src/a.rs");
}
