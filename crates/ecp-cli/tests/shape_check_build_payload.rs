//! Smoke test for the promoted `pub fn build_payload` in `commands::shape_check`.
//!
//! Uses the binary integration pattern (Option A): invokes `ecp shape-check`
//! via the compiled binary and asserts the JSON output shape is intact.
//! The fixture writes a minimal `graph.bin` (same helper as `shape_check_cmd.rs`)
//! and confirms `build_payload` returns `{status, total_fetches, drift_count, drift}`.

use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use serde_json::Value;
use std::process::Command;

mod common;
use common::{ecp_bin, write_graph};

fn build_empty_graph() -> Vec<u8> {
    GraphFixture::new().into_bytes()
}

fn build_graph_with_calls_edge() -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let caller = fx.func("src/a.ts", "foo");
    fx.span(caller, (0, 0, 1, 0));
    let callee = fx.func("src/a.ts", "bar");
    fx.span(callee, (2, 0, 3, 0));
    fx.edge(caller, callee, RelType::Calls);
    fx.into_bytes()
}

#[test]
fn shape_check_build_payload_empty_graph_returns_success_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_graph(dir.path(), &build_empty_graph());

    let out = Command::new(ecp_bin())
        .args([
            "--graph",
            path.to_str().unwrap(),
            "shape-check",
            "--format",
            "json",
        ])
        .output()
        .expect("ecp shape-check failed to spawn");

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

    let out = Command::new(ecp_bin())
        .args([
            "--graph",
            path.to_str().unwrap(),
            "shape-check",
            "--format",
            "json",
        ])
        .output()
        .expect("ecp shape-check failed to spawn");

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
