//! Smoke test for `build_payload` extraction in `commands::impact`.
//!
//! Uses the binary integration pattern (Option A): invokes `cgn impact` with
//! `--format json` and asserts the JSON shape is well-formed. A minimal git
//! repo + index is required so the engine can load the graph.

use std::path::Path;
use std::process::Command;

mod common;
use common::{gnx_bin, run_git};

fn init_repo_and_analyze(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.ts"),
        "export function hello() { return 1; }\n",
    )
    .unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(repo, &["add", "-A"]);
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index spawn failed");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn impact_build_payload_unknown_symbol_returns_error_field() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Graph is indexed but symbol doesn't exist → build_payload returns
    // {"error": "No symbol named … found in graph"}, not a process failure.
    let out = Command::new(gnx_bin())
        .args(["impact", "__nonexistent_symbol_xyzzy__", "--format", "json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("cgn impact failed to spawn");

    assert!(
        out.status.success(),
        "cgn impact should exit 0 even for unknown symbol: stderr={}",
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

    // build_payload returns {"error": "..."} — not a panic, not empty.
    assert!(
        val.get("error").is_some(),
        "expected `error` field in payload for unknown symbol, got: {val}"
    );
}

#[test]
fn impact_build_payload_no_args_returns_error() {
    let tmp = tempfile::tempdir().unwrap();

    // Neither positional name nor --baseline → build_payload returns an
    // InvalidArgument error which propagates to the CLI as a non-zero exit.
    let out = Command::new(gnx_bin())
        .args(["impact"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("cgn impact failed to spawn");

    // clap or build_payload produces a non-zero exit code.
    assert!(
        !out.status.success(),
        "expected failure with no symbol or --baseline, but process succeeded"
    );
}
