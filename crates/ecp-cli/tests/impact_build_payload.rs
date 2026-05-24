//! Smoke test for `build_payload` extraction in `commands::impact`.
//!
//! Uses the binary integration pattern (Option A): invokes `ecp impact` with
//! `--format json` and asserts the JSON shape is well-formed. A minimal git
//! repo + index is required so the engine can load the graph.

use std::path::Path;
use std::process::Command;

mod common;
use common::{ecp_bin, run_git};

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
    let out = Command::new(ecp_bin())
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
fn impact_unknown_symbol_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Graph is indexed but symbol doesn't exist. This must be a process
    // failure so MCP wraps it as isError=true instead of a successful payload.
    let out = Command::new(ecp_bin())
        .args(["impact", "__nonexistent_symbol_xyzzy__", "--format", "json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp impact failed to spawn");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "unknown symbol must exit non-zero so MCP marks isError=true"
    );
    assert!(
        stderr.contains("No symbol named") && stderr.contains("ecp find"),
        "expected actionable unknown-symbol error, got:\n{stderr}"
    );
}

#[test]
fn impact_build_payload_no_args_returns_error() {
    let tmp = tempfile::tempdir().unwrap();

    // Neither positional name nor --baseline → build_payload returns an
    // InvalidArgument error which propagates to the CLI as a non-zero exit.
    let out = Command::new(ecp_bin())
        .args(["impact"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp impact failed to spawn");

    // clap or build_payload produces a non-zero exit code.
    assert!(
        !out.status.success(),
        "expected failure with no symbol or --baseline, but process succeeded"
    );
}
