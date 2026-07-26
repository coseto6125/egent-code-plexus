//! Integration tests for `ecp shape_check`.
//!
//! Each test hand-rolls a minimal `ZeroCopyGraph` (one consumer node,
//! one route node, one Fetches edge) so the drift logic is exercised
//! without any analyzer / extractor in the loop. The graph is written
//! to a tempdir's `graph.bin`, then invoked via the compiled `ecp`
//! binary with `--graph <path>` so we test the full CLI wire-up,
//! clap parsing, and emit() output path.

use ecp_analyzer::fetch_shape::format_reason;
use ecp_core::graph::{NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Construct a 2-node graph: a consumer Function and a Route. Edges
/// supplied by the caller (so each test can vary the rel_type / reason).
/// `route_shape_keys` decides what the route advertises; `None` means
/// no shape extracted (skipped in shape_check).
fn build_graph(
    edges_spec: &[(u32, u32, RelType, &str)],
    route_shape_keys: Option<(Vec<&str>, Vec<&str>)>,
) -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let consumer = fx.func("src/consumer.ts", "fetchUser");
    fx.span(consumer, (1, 0, 5, 0));
    let route = fx.node(NodeKind::Route, "src/api.ts", "GET /users/:id");
    fx.span(route, (1, 0, 5, 0));

    for &(src, tgt, rel, reason) in edges_spec {
        fx.edge_with(src, tgt, rel, 1.0, reason);
    }

    if let Some((resp, err)) = route_shape_keys {
        fx.route_shape(route, &resp, &err);
    }

    fx.into_bytes()
}

fn write_graph(dir: &Path, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join("graph.bin");
    std::fs::write(&p, bytes).unwrap();
    p
}

fn run_shape_check(graph_path: &Path, format: &str) -> (String, String, bool) {
    let out = Command::new(ecp_bin())
        .args([
            "--graph",
            graph_path.to_str().unwrap(),
            "shape-check",
            "--format",
            format,
        ])
        .output()
        .expect("ecp spawn failed");
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

/// PR audit M1/M2 — payload must always surface silent-drop counts
/// (`unparseable_fetches`, `unknown_target_shapes`) so an LLM that reads
/// `drift_count: 0` doesn't infer "everything checks out" when in fact
/// some Fetches edges couldn't be checked at all.
#[test]
fn shape_check_surfaces_unknown_target_shapes() {
    // Fetches edge → Route node with NO RouteShape attached. The old
    // behaviour was `continue` on `shapes.get().is_none()`; the new
    // contract drops a record into `unknown_target_shapes` so the LLM
    // can see "checked? no — shape unknown".
    let dir = tempdir().unwrap();
    let reason = format_reason(&["id".to_string()], 1);
    let bytes = build_graph(&[(0, 1, RelType::Fetches, &reason)], None);
    let path = write_graph(dir.path(), &bytes);

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

    // M1 — always-written counter (safe default 0).
    let unparseable = json["unparseable_fetches"]
        .as_u64()
        .expect("payload must carry `unparseable_fetches`");
    assert_eq!(
        unparseable, 0,
        "reason was well-formed → unparseable should be 0, got {unparseable}"
    );

    // M2 — always-written list (safe default []). One Fetches edge,
    // route has no shape ⇒ exactly one entry.
    let unknown = json["unknown_target_shapes"]
        .as_array()
        .expect("payload must carry `unknown_target_shapes`");
    assert_eq!(
        unknown.len(),
        1,
        "expected one unknown-shape entry, got: {json}"
    );
    assert!(
        unknown[0]["route_uid"].is_string(),
        "entry must carry route_uid: {}",
        unknown[0]
    );
    assert_eq!(
        unknown[0]["route_name"].as_str(),
        Some("GET /users/:id"),
        "entry must carry route_name from the graph: {}",
        unknown[0]
    );

    // Sanity: when target has no shape, the edge is NOT counted as
    // drift (we couldn't check). drift_count must be 0.
    assert_eq!(json["drift_count"].as_u64().unwrap(), 0);
    assert_eq!(json["total_fetches"].as_u64().unwrap(), 1);
}

/// PR audit M1/M2 — safe-default presence on a graph with no
/// silent-drop conditions hit. Confirms the fields always appear
/// regardless of whether they fire.
#[test]
fn shape_check_safe_defaults_appear_with_clean_graph() {
    let dir = tempdir().unwrap();
    let reason = format_reason(&["id".to_string()], 1);
    let bytes = build_graph(
        &[(0, 1, RelType::Fetches, &reason)],
        Some((vec!["id"], vec!["msg"])),
    );
    let path = write_graph(dir.path(), &bytes);

    let (stdout, stderr, ok) = run_shape_check(&path, "json");
    assert!(ok, "command failed: stderr={stderr}");
    let json: Value = {
        let s = stdout.trim();
        let start = s
            .find('{')
            .unwrap_or_else(|| panic!("non-JSON stdout: {s}"));
        serde_json::from_str(&s[start..]).unwrap()
    };

    assert_eq!(json["unparseable_fetches"].as_u64().unwrap(), 0);
    assert_eq!(json["unknown_target_shapes"].as_array().unwrap().len(), 0);
}
