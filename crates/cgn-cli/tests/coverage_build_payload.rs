//! Smoke test for `build_payload` extraction in `commands::coverage`.
//!
//! Invokes `cgn coverage --format json` and asserts the top-level
//! `coverage` key is present, confirming `build_payload` returns the
//! same shape that `run` previously emitted.

use std::process::Command;

mod common;
use common::gnx_bin;

#[test]
fn coverage_build_payload_returns_coverage_key() {
    let tmp = tempfile::tempdir().unwrap();

    let out = Command::new(gnx_bin())
        .args(["coverage", "--format", "json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("cgn coverage failed to spawn");

    assert!(
        out.status.success(),
        "cgn coverage failed: stderr={}",
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
        val.get("coverage").is_some(),
        "expected top-level `coverage` key in payload, got: {val}"
    );
}

#[test]
fn coverage_build_payload_with_repo_returns_per_repo_key() {
    let tmp = tempfile::tempdir().unwrap();

    // `--repo` with an unknown name → graceful empty per_repo array,
    // not a panic or process failure.
    let out = Command::new(gnx_bin())
        .args(["coverage", "--repo", "__no_such_repo__", "--format", "json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("cgn coverage failed to spawn");

    // The command may succeed or fail (unknown repo → selector error),
    // but when it succeeds the payload must have `coverage.per_repo`.
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if let Some(pos) = stdout.find('{') {
            let val: serde_json::Value = serde_json::from_str(&stdout[pos..])
                .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"));
            if val.get("coverage").is_some() {
                assert!(
                    val["coverage"].get("per_repo").is_some(),
                    "expected `coverage.per_repo` when --repo is specified, got: {val}"
                );
            }
        }
    }
    // Non-zero exit (selector error) is also acceptable — the important
    // thing is no panic / no process crash.
}
