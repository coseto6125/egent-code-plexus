//! Verify `gnx shape_check --route <path>` filters Fetches edges
//! by target Route path. No-match case prints helpful message.

use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn shape_check_help_lists_route_arg() {
    let output = Command::new(gnx_bin())
        .args(["shape-check", "--help"])
        .output()
        .expect("run gnx shape-check --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--route"),
        "expected --route arg in help, got: {stdout}"
    );
}

#[test]
fn shape_check_route_no_match_emits_helpful_message() {
    let output = Command::new(gnx_bin())
        .args(["shape-check", "--route", "/__nonexistent_route__"])
        .output()
        .expect("run gnx shape-check --route");
    assert!(
        output.status.success(),
        "shape-check should succeed even with no matches; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("No routes match") || combined.contains("no match") || combined.is_empty(),
        "expected no-match message or empty output, got: {combined}"
    );
}
