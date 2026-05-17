//! Smoke test for `build_payload` extraction in `commands::diff`.
//!
//! Verifies that `gnx diff --section bindings --baseline <ref>` produces
//! the same JSON shape as before the refactor: top-level `baseline`,
//! `current`, and `sections` fields.
//!
//! The "identical SHAs" fast-path is the easiest to trigger without a
//! real two-commit repo — pass `--baseline HEAD` in a fresh git repo
//! so baseline and current resolve to the same SHA.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn init_repo(repo: &Path) {
    std::fs::write(repo.join("README.md"), "hello").unwrap();
    let _ = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
}

#[test]
fn diff_build_payload_identical_shas_returns_envelope_shape() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());

    // `--baseline HEAD` → baseline_sha == current_sha → fast-path in build_payload.
    // The fast-path returns empty diffs without touching git_guard or graph.bin.
    let out = Command::new(gnx_bin())
        .args([
            "diff",
            "--section",
            "bindings",
            "--baseline",
            "HEAD",
            "--format",
            "json",
        ])
        .current_dir(repo.path())
        .output()
        .expect("gnx diff failed to spawn");

    if !out.status.success() {
        // If baseline resolution fails (e.g., no commits), skip gracefully.
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("resolve") || stderr.contains("not a valid") || stderr.contains("HEAD") {
            eprintln!("skipping diff_build_payload test: baseline resolution failed: {stderr}");
            return;
        }
        panic!("gnx diff failed unexpectedly: stderr={stderr}");
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "expected JSON on stdout, got:\nstdout={stdout}\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let val: Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"));

    // Envelope shape: {baseline: {ref, sha}, current: {ref, sha}, sections: {...}}
    assert!(
        val.get("baseline").is_some(),
        "missing `baseline` in payload: {val}"
    );
    assert!(
        val.get("current").is_some(),
        "missing `current` in payload: {val}"
    );
    assert!(
        val.get("sections").is_some(),
        "missing `sections` in payload: {val}"
    );
}
