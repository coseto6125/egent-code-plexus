//! `shape-check --route X` must filter routes before it counts them.
//! Contract: a route that matches the filter but has no extracted shape is
//! reported as "no shape" (`unknown_target_shapes`), never as "No routes match".

use ecp_analyzer::fetch_shape::format_reason;
use ecp_core::graph::{NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
use serde_json::Value;
use std::process::Command;
use tempfile::tempdir;

fn run(with_shape: bool, route_filter: &str) -> (Value, String) {
    let mut fx = GraphFixture::new();
    let consumer = fx.func("src/consumer.ts", "fetchUser");
    fx.span(consumer, (1, 0, 5, 0));
    let route = fx.node(NodeKind::Route, "src/api.ts", "GET /users/:id");
    fx.span(route, (1, 0, 5, 0));
    fx.edge_with(
        consumer,
        route,
        RelType::Fetches,
        1.0,
        &format_reason(&["id".to_string()], 1),
    );
    if with_shape {
        fx.route_shape(route, &["id"], &["msg"]);
    }
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, fx.into_bytes()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args([
            "--graph",
            path.to_str().unwrap(),
            "shape-check",
            "--route",
            route_filter,
            "--format",
            "json",
        ])
        .output()
        .expect("spawn ecp");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let start = stdout.find('{').expect("json stdout");
    (
        serde_json::from_str(&stdout[start..]).unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn test_shape_check_route_filter_matching_route_without_shape_reports_no_shape() {
    let (json, stderr) = run(false, "/users");
    assert!(
        !stderr.contains("No routes match"),
        "matching route must not print the no-match hint: {stderr}"
    );
    assert_eq!(json["unknown_target_shapes"].as_array().unwrap().len(), 1);
    assert_eq!(json["total_fetches"].as_u64().unwrap(), 1);
}

#[test]
fn test_shape_check_route_filter_nonmatching_route_still_reports_no_match() {
    let (json, stderr) = run(true, "/__absent__");
    assert!(stderr.contains("No routes match"), "stderr: {stderr}");
    assert_eq!(json["total_fetches"].as_u64().unwrap(), 0);
}
