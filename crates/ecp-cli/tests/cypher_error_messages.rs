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
/// status — callers decide whether they expect success or failure.
fn run_capture(repo: &std::path::Path, query: &str) -> (std::process::ExitStatus, String) {
    let (status, _stdout, stderr) = run_capture_full(repo, query);
    (status, stderr)
}

/// Run a cypher query and return (exit status, stdout, stderr).
fn run_capture_full(
    repo: &std::path::Path,
    query: &str,
) -> (std::process::ExitStatus, String, String) {
    let out = Command::new(ecp_bin())
        .args(["cypher", query, "--format", "json"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status, stdout, stderr)
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

/// Unknown property with a close match (`n.file` → `filePath`): must fail
/// with non-zero exit. The suggestion must appear in both stderr AND the stdout
/// JSON payload, so LLM consumers reading only stdout see the error, not empty
/// rows (which are indistinguishable from a genuine no-match).
#[test]
fn unknown_property_with_suggestion_fails_with_error() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let (status, stdout, stderr) = run_capture_full(
        tmp.path(),
        "MATCH (n) WHERE n.file CONTAINS 'x' RETURN n.name",
    );

    assert!(
        !status.success(),
        "unknown-property query must fail (exit non-zero); stderr={stderr} stdout={stdout}"
    );
    // Suggestion must be visible in stdout payload so LLM agents see it.
    assert!(
        stdout.contains("filePath"),
        "stdout must carry the did-you-mean suggestion 'filePath': {stdout}"
    );
    assert!(
        stdout.contains("file"),
        "stdout must name the unknown property 'file': {stdout}"
    );
    // Stderr carries the human-readable "Command failed:" line.
    assert!(
        stderr.contains("unknown") || stderr.contains("filePath"),
        "stderr must reference the error: {stderr}"
    );
}

/// Unknown property with NO close match: must fail and list known properties in
/// the stdout payload so the LLM can pick the right one.
#[test]
fn unknown_property_no_suggestion_fails_listing_known() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // "xyzzy" is far enough from every known property that no suggestion fires.
    let (status, stdout, stderr) = run_capture_full(tmp.path(), "MATCH (n) RETURN n.xyzzy");

    assert!(
        !status.success(),
        "unknown-property query must fail; stderr={stderr} stdout={stdout}"
    );
    assert!(
        stdout.contains("xyzzy"),
        "stdout must name the unknown property 'xyzzy': {stdout}"
    );
    // At least one known property must appear in the stdout payload.
    assert!(
        stdout.contains("name") || stdout.contains("filePath") || stdout.contains("startLine"),
        "stdout must list known properties: {stdout}"
    );
}

/// Legal properties — including `startLine` and camelCase flag aliases —
/// must not fail. Guards against building the known-set from the stale doc comment.
#[test]
fn known_property_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let (status, stdout, stderr) = run_capture_full(
        tmp.path(),
        "MATCH (n) WHERE n.filePath CONTAINS 'x' RETURN n.startLine",
    );

    assert!(
        status.success(),
        "valid query should succeed; stderr={stderr} stdout={stdout}"
    );
    assert!(
        !stdout.contains("unknown"),
        "valid properties must not trigger an error payload: {stdout}"
    );
}

/// SQL JOIN syntax → hint toward OPTIONAL MATCH.
#[test]
fn left_join_gets_sql_hint() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let (status, stderr) = run_capture(
        tmp.path(),
        "MATCH (f) WITH f LEFT JOIN (x)-[r]->(f) RETURN f",
    );
    assert!(!status.success());
    assert!(stderr.contains("hint:"), "expected a hint, got: {stderr}");
    assert!(
        stderr.contains("OPTIONAL MATCH"),
        "expected OPTIONAL MATCH guidance, got: {stderr}"
    );
}

/// CALL … YIELD stored-procedure syntax → hint toward ecp inspect.
#[test]
fn call_yield_gets_procedure_hint() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let (status, stderr) = run_capture(
        tmp.path(),
        "MATCH (n) CALL ecp.edge_types(n) YIELD relation RETURN n",
    );
    assert!(!status.success());
    assert!(
        stderr.contains("ecp inspect"),
        "expected inspect guidance, got: {stderr}"
    );
}

/// COUNT((pattern)) aggregate → hint toward EXISTS / OPTIONAL MATCH.
#[test]
fn count_pattern_gets_exists_hint() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let (status, stderr) = run_capture(
        tmp.path(),
        "MATCH (f) WITH f, COUNT((x)-[r]->(f)) AS n RETURN f",
    );
    assert!(!status.success());
    assert!(
        stderr.contains("EXISTS") || stderr.contains("OPTIONAL MATCH"),
        "expected exists/optional guidance, got: {stderr}"
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
