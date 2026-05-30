//! E2E tests for `ecp cypher` error paths: parse errors, semantic errors,
//! and caret-pointer formatting in error output.

use std::process::Command;

// Minimal fixture so the CLI has a graph to query against.
const SOURCE: &str = "function foo() { return 1; }\n";

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
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

    let out = Command::new(ecp_bin())
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

/// Run a cypher query and return (exit status, stderr) without asserting on the
/// status — callers decide whether they expect success (warning path) or
/// failure (parse / semantic error path).
fn run_capture(repo: &std::path::Path, query: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(ecp_bin())
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

    let (status, stderr) = run_capture(tmp.path(), "MATCH");

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

    let (status, stderr) = run_capture(tmp.path(), "MATCH (a:Foo) RETURN a");

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

    let (status, stderr) = run_capture(tmp.path(), "MATCH (a)-[r:NOSUCH]->(b) RETURN a, b");

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

/// Unknown property name (`n.file` — the real one is `filePath`) evaluates to
/// Null and silently returns 0 rows. The query still succeeds (exit 0), but a
/// stderr warning must surface the typo + the closest known name, so the caller
/// can tell a typo'd-property empty result from a genuine no-match.
#[test]
fn unknown_property_emits_warning() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let (status, stderr) = run_capture(
        tmp.path(),
        "MATCH (n) WHERE n.file CONTAINS 'x' RETURN n.name",
    );

    assert!(
        status.success(),
        "unknown-property query should still succeed (warning, not error); stderr={stderr}"
    );
    assert!(
        stderr.contains("unknown cypher property") && stderr.contains("file"),
        "stderr should warn about unknown property 'file': {stderr}"
    );
    assert!(
        stderr.contains("filePath"),
        "stderr should suggest the closest known property 'filePath': {stderr}"
    );
}

/// Legal properties — including `startLine` and the camelCase flag aliases that
/// the doc comment omits — must NOT trigger the unknown-property warning.
/// Guards against building the known-set from the stale doc comment.
#[test]
fn known_property_no_warning() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let (status, stderr) = run_capture(
        tmp.path(),
        "MATCH (n) WHERE n.filePath CONTAINS 'x' RETURN n.startLine",
    );

    assert!(
        status.success(),
        "valid query should succeed; stderr={stderr}"
    );
    assert!(
        !stderr.contains("unknown cypher property"),
        "valid properties must not warn (startLine is legal): {stderr}"
    );
}

/// Parse errors must emit a `^` caret pointer indicating the error offset.
#[test]
fn error_includes_caret_pointer() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // `MATCH` alone triggers a parse error which includes an offset → caret.
    let (status, stderr) = run_capture(tmp.path(), "MATCH");

    assert!(!status.success(), "expected non-zero exit; stderr={stderr}");
    assert!(
        stderr.contains('^'),
        "stderr should contain a caret '^' pointing at the error offset:\n{stderr}"
    );
}
