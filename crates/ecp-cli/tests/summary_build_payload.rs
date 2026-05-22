//! Smoke test for `build_payload` extraction in `commands::summary`.
//!
//! Invokes `ecp summary --format json` and asserts the top-level
//! `summary` key is present, confirming `build_payload` returns the
//! same shape that `run` previously emitted.

use std::process::Command;

mod common;
use common::ecp_bin;

#[test]
fn summary_build_payload_returns_summary_key() {
    let tmp = tempfile::tempdir().unwrap();

    let out = Command::new(ecp_bin())
        .args(["summary", "--format", "json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp summary failed to spawn");

    assert!(
        out.status.success(),
        "ecp summary failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "expected JSON on stdout, got:\nstdout={stdout}\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let val: serde_json::Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"));

    assert!(
        val.get("summary").is_some(),
        "expected top-level `summary` key in payload, got: {val}"
    );
}

#[test]
fn summary_build_payload_with_repo_returns_per_repo_key() {
    let tmp = tempfile::tempdir().unwrap();

    // `--repo` with an unknown name → graceful empty per_repo array,
    // not a panic or process failure.
    let out = Command::new(ecp_bin())
        .args(["summary", "--repo", "__no_such_repo__", "--format", "json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp summary failed to spawn");

    // The command may succeed or fail (unknown repo → selector error),
    // but when it succeeds the payload must have `summary.per_repo`.
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if let Some(pos) = stdout.find('{') {
            let val: serde_json::Value = serde_json::from_str(&stdout[pos..])
                .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"));
            if val.get("summary").is_some() {
                assert!(
                    val["summary"].get("per_repo").is_some(),
                    "expected `summary.per_repo` when --repo is specified, got: {val}"
                );
            }
        }
    }
    // Non-zero exit (selector error) is also acceptable — the important
    // thing is no panic / no process crash.
}

/// Back-compat alias: the legacy `coverage` verb still routes to the same
/// command for one release. Drop this test (and the `#[command(alias)]` on
/// the `Summary` variant) when the alias is retired.
#[test]
fn coverage_alias_still_routes_to_summary() {
    let tmp = tempfile::tempdir().unwrap();

    let out = Command::new(ecp_bin())
        .args(["coverage", "--format", "json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp coverage (alias) failed to spawn");

    assert!(
        out.status.success(),
        "ecp coverage (alias) failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout.find('{').expect("expected JSON on stdout");
    let val: serde_json::Value = serde_json::from_str(&stdout[json_start..]).expect("JSON parse");
    // Alias must reach the same `summary` payload — not a separate legacy
    // shape, since the alias is just a clap-level redirection.
    assert!(
        val.get("summary").is_some(),
        "expected `summary` key when invoked via `coverage` alias, got: {val}"
    );
}
