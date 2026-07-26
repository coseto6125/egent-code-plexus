//! Integration tests for `ecp find`.
//!
//! Uses the same minimal-graph fixture pattern as `tests/search_cmd.rs`.
//! 14-language coverage applies to parser / graph primitives — `find` is a
//! CLI-level subcommand, so one focused test suite is sufficient (noted in PR
//! body per CLAUDE.md).

use ecp_core::graph::{FileCategory, NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
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
    let mut fx = GraphFixture::new();

    // A file's category comes from the first NodeSpec that references it —
    // `file_as` leaves an already-registered path's category alone, so
    // registering per-spec in order reproduces that "first wins" rule.
    let mut ids: Vec<u32> = Vec::with_capacity(nodes_spec.len());
    for ns in nodes_spec {
        fx.file_as(ns.file, ns.category);
        let id = fx.node(ns.kind, ns.file, ns.name);
        fx.span(id, (ns.line, 0, ns.line + 10, 0));
        ids.push(id);
    }

    for &(src, tgt) in extra_edges {
        fx.edge_with(ids[src], ids[tgt], RelType::Calls, 1.0, "test");
    }

    let bytes = fx.into_bytes();
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

// ── Task A: signature must never be a bare uid string ─────────────────────────

#[test]
fn find_signature_is_not_a_bare_uid() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "compute_hits",
            kind: NodeKind::Function,
            category: FileCategory::Source,
            file: "src/find.rs",
            line: 10,
        }],
        &[],
    );
    let out = run_find(&graph, &["compute_hits", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], true);
    let sig = json["matches"][0]["signature"].as_str().unwrap();
    // A pure-digit string means a raw uid leaked — the signature must be human-readable.
    assert!(
        !sig.chars().all(|c| c.is_ascii_digit()),
        "signature must not be a bare uid number, got: {sig}"
    );
    // Expect "{Kind} {name}" form.
    assert!(
        sig.contains("compute_hits"),
        "signature must include the symbol name, got: {sig}"
    );
}

#[test]
fn find_signature_contains_kind_and_name() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "MyStruct",
            kind: NodeKind::Struct,
            category: FileCategory::Source,
            file: "src/types.rs",
            line: 5,
        }],
        &[],
    );
    let out = run_find(&graph, &["MyStruct", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    let sig = json["matches"][0]["signature"].as_str().unwrap();
    // Must be "Struct MyStruct" form.
    assert_eq!(sig, "Struct MyStruct", "unexpected signature: {sig}");
}

// ── Task B: --file filter ─────────────────────────────────────────────────────

#[test]
fn find_file_filter_picks_correct_file() {
    // Two functions named `connect` in different files; --file should pick the right one.
    let (_dir, graph) = build_graph(
        &[
            NodeSpec {
                name: "connect",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/db.rs",
                line: 1,
            },
            NodeSpec {
                name: "connect",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/network.rs",
                line: 1,
            },
        ],
        &[],
    );
    let out = run_find(&graph, &["connect", "--file", "db", "--format", "json"]);
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], true);
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1, "file filter should narrow to one match");
    assert_eq!(matches[0]["file"], "src/db.rs");
}

#[test]
fn find_file_filter_no_match_returns_found_false() {
    let (_dir, graph) = build_graph(
        &[NodeSpec {
            name: "connect",
            kind: NodeKind::Function,
            category: FileCategory::Source,
            file: "src/db.rs",
            line: 1,
        }],
        &[],
    );
    // --file substring that matches nothing → honest found:false, no fabrication.
    let out = run_find(
        &graph,
        &["connect", "--file", "nonexistent_path", "--format", "json"],
    );
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], false);
    assert_eq!(json["matches"].as_array().unwrap().len(), 0);
}

#[test]
fn find_file_filter_composes_with_kind() {
    // Two matches for "process": one Function in proc.rs, one Method in handler.rs.
    // --file proc --kind function should match only the Function.
    let (_dir, graph) = build_graph(
        &[
            NodeSpec {
                name: "process",
                kind: NodeKind::Function,
                category: FileCategory::Source,
                file: "src/proc.rs",
                line: 1,
            },
            NodeSpec {
                name: "process",
                kind: NodeKind::Method,
                category: FileCategory::Source,
                file: "src/handler.rs",
                line: 10,
            },
        ],
        &[],
    );
    let out = run_find(
        &graph,
        &[
            "process", "--file", "proc", "--kind", "function", "--format", "json",
        ],
    );
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    assert_eq!(json["found"], true);
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["file"], "src/proc.rs");
    assert_eq!(matches[0]["kind"], "Function");
}
