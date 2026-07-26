//! Integration tests for `ecp processes` (FU-013).
//!
//! Mirrors the synthetic-graph pattern from `find_cmd.rs`: build a tiny
//! `ZeroCopyGraph` containing one Process node + 3 member Functions,
//! serialise to `graph.bin`, then spawn `ecp processes ...` against it
//! via `--graph <path>`.
//!
//! The CLI surface is one top-level command + one subcommand (`trace`).
//! 14-language coverage doesn't apply — Process emission is post-process,
//! parser-agnostic (driven by Calls edges).

use ecp_core::graph::{NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Build a graph with `member_count` Function members + 1 Process node
/// whose trace covers all members. `process_label` is the Process name
/// (`"Entry → Terminal"` shape). `communities` aligns 1-1 with members;
/// pass distinct values to exercise the cross-vs-intra classifier.
fn build_process_graph(
    process_label: &str,
    member_names: &[&str],
    communities: &[u16],
) -> (TempDir, PathBuf) {
    assert_eq!(
        member_names.len(),
        communities.len(),
        "fixture length mismatch"
    );
    let dir = TempDir::new().unwrap();
    let mut fx = GraphFixture::new();

    let ids: Vec<u32> = member_names
        .iter()
        .zip(communities.iter())
        .enumerate()
        .map(|(i, (name, comm))| {
            let id = fx.func("src/lib.rs", name);
            fx.span(id, ((i * 10) as u32 + 1, 0, (i * 10) as u32 + 5, 0));
            fx.community(id, *comm);
            id
        })
        .collect();

    let process = fx.process("src/lib.rs", process_label, &ids);
    fx.span(process, (1, 0, 5, 0));
    fx.community(process, communities[0]);

    for &id in &ids {
        fx.edge_with(id, process, RelType::StepInProcess, 1.0, "step:test");
    }

    let bytes = fx.into_bytes();
    let graph_path = dir.path().join("graph.bin");
    std::fs::write(&graph_path, &bytes).unwrap();
    (dir, graph_path)
}

fn run_processes(graph: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ecp_bin())
        .arg("processes")
        .args(args)
        .arg("--graph")
        .arg(graph)
        .arg("--format")
        .arg("json")
        .output()
        .expect("ecp processes spawn")
}

fn parse_json_stdout(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout: {stdout}"));
    serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|e| panic!("JSON parse error: {e}\nstdout: {stdout}"))
}

#[test]
fn list_returns_process_label_and_step_count() {
    let (_dir, graph) = build_process_graph(
        "Authenticate → IssueToken",
        &["authenticate", "verify_password", "issue_token"],
        &[1, 1, 1],
    );

    // --all bypasses the default min-signal filter so a 3-step intra process is shown.
    let out = run_processes(&graph, &["--all"]);
    assert!(
        out.status.success(),
        "ecp processes exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload = parse_json_stdout(&out);
    assert_eq!(payload["status"], "success");
    assert_eq!(payload["total"], 1);
    let results = payload["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["label"], "Authenticate → IssueToken");
    assert_eq!(results[0]["step_count"], 3);
    assert_eq!(results[0]["process_type"], "intra_community");
}

#[test]
fn list_cross_community_classification() {
    let (_dir, graph) = build_process_graph(
        "EntryFn → TerminalFn",
        &["entry_fn", "middle_fn", "terminal_fn"],
        &[1, 2, 1], // distinct communities → cross
    );
    let payload = parse_json_stdout(&run_processes(&graph, &[]));
    assert_eq!(payload["results"][0]["process_type"], "cross_community");
}

#[test]
fn trace_substring_match_emits_ordered_steps() {
    let (_dir, graph) = build_process_graph(
        "HandleRequest → WriteResponse",
        &["handle_request", "parse_body", "write_response"],
        &[1, 1, 1],
    );

    // Match by substring of the label (case-insensitive).
    let out = run_processes(&graph, &["trace", "handlerequest"]);
    assert!(
        out.status.success(),
        "ecp processes trace exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload = parse_json_stdout(&out);
    assert_eq!(payload["status"], "success");
    assert_eq!(payload["matched"], 1);

    let steps = payload["results"][0]["steps"]
        .as_array()
        .expect("steps array");
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0]["name"], "handle_request");
    assert_eq!(steps[1]["name"], "parse_body");
    assert_eq!(steps[2]["name"], "write_response");
    assert_eq!(steps[0]["step"], 1);
    assert_eq!(steps[2]["step"], 3);
}

#[test]
fn trace_no_match_returns_not_found() {
    let (_dir, graph) = build_process_graph("Foo → Bar", &["foo", "mid", "bar"], &[1, 1, 1]);
    let out = run_processes(&graph, &["trace", "nonexistent-process-pattern"]);
    assert!(out.status.success());
    let payload = parse_json_stdout(&out);
    assert_eq!(payload["status"], "not_found");
}

/// Build a graph where non-Process nodes (mimicking the PathLiteral / File
/// nodes that later builder passes append) follow the single Process node.
/// This breaks the "everything after process_start is a Process" assumption,
/// which used to make `processes` index `traces_offsets[k+1]` out of bounds
/// once the limit reached past the real process count (a small real repo:
/// `total` was over-counted as `nodes.len() - process_start`, and listing
/// with `--limit` ≥ that miscount panicked at `traces_offsets[k+1]`).
fn build_graph_with_trailing_non_process_nodes() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let mut fx = GraphFixture::new();

    let member_names = ["entry", "middle", "terminal"];
    let ids: Vec<u32> = member_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let id = fx.func("src/lib.rs", name);
            fx.span(id, ((i * 10) as u32 + 1, 0, (i * 10) as u32 + 5, 0));
            fx.community(id, 1);
            id
        })
        .collect();

    let process = fx.process("src/lib.rs", "Entry → Terminal", &ids);
    fx.span(process, (1, 0, 5, 0));
    fx.community(process, 1);

    // Trailing non-Process nodes after the single Process — the regression: a
    // naive `nodes.len() - process_start` would count these as processes.
    for i in 0..20 {
        let p = fx.node(NodeKind::PathLiteral, "src/lib.rs", &format!("path/{i}"));
        fx.span(p, (1, 0, 1, 0));
    }

    let bytes = fx.into_bytes();
    let graph_path = dir.path().join("graph.bin");
    std::fs::write(&graph_path, &bytes).unwrap();
    (dir, graph_path)
}

#[test]
fn list_does_not_overcount_or_panic_with_trailing_non_process_nodes() {
    let (_dir, graph) = build_graph_with_trailing_non_process_nodes();
    // A limit far past the real process count (1) used to walk into the
    // trailing PathLiteral nodes and panic at `traces_offsets[k+1]`.
    // --all bypasses the min-signal filter so the 3-step intra process is shown.
    let out = run_processes(&graph, &["--limit", "100", "--all"]);
    assert!(
        out.status.success(),
        "ecp processes panicked on trailing non-Process nodes: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload = parse_json_stdout(&out);
    assert_eq!(payload["status"], "success");
    // total is the true process count (1), not nodes.len() - process_start (21).
    assert_eq!(payload["total"], 1);
    assert_eq!(payload["shown"], 1);
}

#[test]
fn trace_does_not_panic_with_trailing_non_process_nodes() {
    let (_dir, graph) = build_graph_with_trailing_non_process_nodes();
    let out = run_processes(&graph, &["trace", "entry", "--limit", "100"]);
    assert!(
        out.status.success(),
        "ecp processes trace panicked: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload = parse_json_stdout(&out);
    assert_eq!(payload["status"], "success");
    assert_eq!(payload["matched"], 1);
}

// ── min-signal filter tests ────────────────────────────────────────────────

/// Build a graph with multiple Process nodes at varying step_counts and
/// community configurations. Used to verify the default min-signal filter.
///
/// Processes inserted (all intra_community unless noted):
///   P0 "Noise3"        — 3 steps, intra_community  → should be filtered
///   P1 "Noise5"        — 5 steps, intra_community  → should be filtered
///   P2 "CrossShort3"   — 3 steps, cross_community  → should pass (cross)
///   P3 "LongIntra8"    — 8 steps, intra_community  → should pass (long)
///   P4 "LongIntra7"    — 7 steps, intra_community  → should pass (>=7)
fn build_multi_process_graph() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let mut fx = GraphFixture::new();

    // Pre-allocate member functions (indices 0..N) then Process nodes.
    // Communities: comm=1 for intra, comms 1+2 for cross.
    struct ProcSpec {
        label: &'static str,
        steps: usize,
        cross: bool,
    }
    let specs = [
        ProcSpec {
            label: "Noise3 → End",
            steps: 3,
            cross: false,
        },
        ProcSpec {
            label: "Noise5 → End",
            steps: 5,
            cross: false,
        },
        ProcSpec {
            label: "CrossShort3 → End",
            steps: 3,
            cross: true,
        },
        ProcSpec {
            label: "LongIntra8 → End",
            steps: 8,
            cross: false,
        },
        ProcSpec {
            label: "LongIntra7 → End",
            steps: 7,
            cross: false,
        },
    ];

    // Track each process's member node indices so we can build the trace
    // and the StepInProcess edges once every Process node exists.
    let mut member_ranges: Vec<Vec<u32>> = Vec::new();
    let mut total_members: u32 = 0;

    for spec in &specs {
        let mut ids = Vec::new();
        for j in 0..spec.steps as u32 {
            let comm: u16 = if spec.cross && j == 1 { 2 } else { 1 };
            let name_str = format!(
                "fn_{}_{}",
                spec.label.split_whitespace().next().unwrap_or("x"),
                j
            );
            let id = fx.func("src/lib.rs", &name_str);
            fx.span(id, (total_members * 10 + 1, 0, total_members * 10 + 5, 0));
            fx.community(id, comm);
            ids.push(id);
            total_members += 1;
        }
        member_ranges.push(ids);
    }

    let mut proc_ids = Vec::new();
    for (spec, ids) in specs.iter().zip(member_ranges.iter()) {
        let p = fx.process("src/lib.rs", spec.label, ids);
        fx.span(p, (1, 0, 5, 0));
        fx.community(p, 1);
        proc_ids.push(p);
    }

    for (ids, &proc_idx) in member_ranges.iter().zip(proc_ids.iter()) {
        for &idx in ids {
            fx.edge_with(idx, proc_idx, RelType::StepInProcess, 1.0, "step:test");
        }
    }

    let bytes = fx.into_bytes();
    let graph_path = dir.path().join("graph.bin");
    std::fs::write(&graph_path, &bytes).unwrap();
    (dir, graph_path)
}

/// Default output silently drops low-signal (intra, step_count < 7) rows and
/// reports `filtered` count so the LLM knows truncation happened.
///
/// Fixture has 5 processes: 2 noise (intra, <7 steps) → filtered;
/// 3 signal (cross or ≥7 steps) → shown.
#[test]
fn default_filter_drops_noise_and_emits_filtered_count() {
    let (_dir, graph) = build_multi_process_graph();
    let payload = parse_json_stdout(&run_processes(&graph, &[]));

    assert_eq!(payload["status"], "success");
    // total = all 5 processes in graph
    assert_eq!(payload["total"], 5, "total must count all graph processes");
    // filtered = 2 noise rows dropped
    let filtered = payload["filtered"]
        .as_u64()
        .expect("payload must carry `filtered` (u64)");
    assert_eq!(
        filtered, 2,
        "2 intra+short processes should be filtered: {payload}"
    );
    // shown = 3 signal processes
    let results = payload["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        3,
        "3 signal processes should be shown: {payload}"
    );

    // None of the shown rows should be the noise labels.
    for r in results {
        let label = r["label"].as_str().unwrap_or("");
        assert!(
            !label.starts_with("Noise"),
            "noise process leaked into default output: {label}"
        );
    }
}

/// `--all` disables the filter and returns every process; `filtered` is 0.
#[test]
fn all_flag_disables_filter_returns_every_process() {
    let (_dir, graph) = build_multi_process_graph();
    let payload = parse_json_stdout(&run_processes(&graph, &["--all"]));

    assert_eq!(payload["status"], "success");
    assert_eq!(payload["total"], 5);
    let filtered = payload["filtered"]
        .as_u64()
        .expect("payload must carry `filtered` even with --all");
    assert_eq!(filtered, 0, "`filtered` must be 0 when --all is passed");
    let results = payload["results"].as_array().expect("results array");
    assert_eq!(results.len(), 5, "--all must return all 5 processes");
}

/// `processes trace <pattern>` is not affected by the min-signal filter:
/// matching a noise label must still return steps.
#[test]
fn trace_unaffected_by_signal_filter() {
    let (_dir, graph) = build_multi_process_graph();
    // "Noise3" has only 3 steps and is intra — would be filtered in list view.
    let out = run_processes(&graph, &["trace", "noise3"]);
    assert!(
        out.status.success(),
        "ecp processes trace failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload = parse_json_stdout(&out);
    // trace bypasses the filter — must still find and return the process.
    assert_eq!(
        payload["status"], "success",
        "trace must find Noise3 even though list would filter it: {payload}"
    );
    let steps = payload["results"][0]["steps"]
        .as_array()
        .expect("steps array");
    assert_eq!(steps.len(), 3, "Noise3 has 3 steps");
}
