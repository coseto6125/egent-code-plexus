//! E2E tests for `gnx cypher` error paths: parse errors, semantic errors,
//! and caret-pointer formatting in error output.

use std::process::Command;

// Minimal fixture so the CLI has a graph to query against.
const SOURCE: &str = "function foo() { return 1; }\n";

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn init_repo_and_analyze(repo: &std::path::Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/err.ts"), SOURCE).unwrap();

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

    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run a cypher query that is expected to fail; return stderr.
fn run_expect_failure(repo: &std::path::Path, query: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(gnx_bin())
        .args(["cypher", query, "--format", "json"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status, stderr)
}

/// `MATCH` alone (no pattern) → parse error, non-zero exit.
#[test]
fn parse_error_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let (status, stderr) = run_expect_failure(tmp.path(), "MATCH");

    assert!(
        !status.success(),
        "expected non-zero exit for malformed query, got success; stderr={stderr}"
    );
    let stderr_lower = stderr.to_lowercase();
    assert!(
        stderr_lower.contains("parse error") || stderr_lower.contains("error"),
        "stderr should contain 'parse error' or 'error': {stderr}"
    );
}

/// Unknown node label `Foo` → semantic error about NodeKind.
#[test]
fn semantic_unknown_nodekind() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let (status, stderr) = run_expect_failure(tmp.path(), "MATCH (a:Foo) RETURN a");

    assert!(
        !status.success(),
        "expected non-zero exit for unknown NodeKind; stderr={stderr}"
    );
    let stderr_lower = stderr.to_lowercase();
    assert!(
        stderr_lower.contains("unknown") || stderr_lower.contains("nodekind"),
        "stderr should mention unknown NodeKind: {stderr}"
    );
}

/// Unknown relationship type `NOSUCH` → semantic error about RelType.
#[test]
fn semantic_unknown_reltype() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let (status, stderr) = run_expect_failure(tmp.path(), "MATCH (a)-[r:NOSUCH]->(b) RETURN a, b");

    assert!(
        !status.success(),
        "expected non-zero exit for unknown RelType; stderr={stderr}"
    );
    let stderr_lower = stderr.to_lowercase();
    assert!(
        stderr_lower.contains("unknown") || stderr_lower.contains("reltype"),
        "stderr should mention unknown RelType: {stderr}"
    );
}

/// Parse errors must emit a `^` caret pointer indicating the error offset.
#[test]
fn error_includes_caret_pointer() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // `MATCH` alone triggers a parse error which includes an offset → caret.
    let (status, stderr) = run_expect_failure(tmp.path(), "MATCH");

    assert!(!status.success(), "expected non-zero exit; stderr={stderr}");
    assert!(
        stderr.contains('^'),
        "stderr should contain a caret '^' pointing at the error offset:\n{stderr}"
    );
}
