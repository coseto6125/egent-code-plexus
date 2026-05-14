//! Integration test for `gnx detect-changes` (Plan B — symbol-diff path).
//!
//! Pins down the post-B behaviours:
//!   1. **Added symbol detection**: a function that didn't exist in the old
//!      graph but appears in the re-parse must be flagged `change_type=added`.
//!   2. **No phantom matches**: symbols that merely got shifted in line number
//!      (because earlier code expanded) but whose body is unchanged must NOT
//!      appear in `changed_symbols`. Pre-B used hunk overlap and would
//!      incorrectly flag them.
//!   3. **Body-change detection via content_hash**: a function whose
//!      identity (name/kind/file) is unchanged but body differs must be
//!      flagged `change_type=modified`, not "unchanged".
//!   4. **Process filter**: virtual aggregates (`Process`/`File`) must never
//!      appear in `changed_symbols`.
//!   5. **risk bucket monotonicity** + empty-diff sentinel.
//!   6. **Compact output format**: default folded compact for LLM review.

use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

const ORIGINAL: &str = include_str!("fixtures/auth.original.ts");
const MODIFIED: &str = include_str!("fixtures/auth.modified.ts");

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_gnx_json(repo: &Path, home: &Path, args: &[&str]) -> Value {
    let out = Command::new(gnx_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("gnx failed to spawn");
    assert!(
        out.status.success(),
        "gnx {args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("gnx {args:?} did not return JSON: {e}\nstdout was: {stdout}"))
}

fn run_gnx_text(repo: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(gnx_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("gnx failed to spawn");
    assert!(
        out.status.success(),
        "gnx {args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn init_repo_and_analyze(repo: &Path, home: &Path) {
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/auth.ts"), ORIGINAL).unwrap();

    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/detect-changes-test.git",
        ],
    );
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
        .args(["analyze", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("analyze failed to spawn");
    assert!(
        out.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn detect_changes_with_real_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo_and_analyze(repo, home_tmp.path());

    // Apply the modification (adds `isRateLimited` + call site inside handleLogin).
    std::fs::write(repo.join("src/auth.ts"), MODIFIED).unwrap();

    let result = run_gnx_json(
        repo,
        home_tmp.path(),
        &["detect-changes", "--repo", ".", "--format", "json"],
    );

    let symbols = result["changed_symbols"]
        .as_array()
        .expect("changed_symbols is array");

    let names: Vec<&str> = symbols
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    let change_types: HashMap<&str, &str> = symbols
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap(),
                s["change_type"].as_str().unwrap(),
            )
        })
        .collect();

    // ── (1) handleLogin's body actually changed → must be `modified`.
    assert!(
        names.contains(&"handleLogin"),
        "expected handleLogin in changed_symbols, got {names:?}"
    );
    assert_eq!(
        change_types.get("handleLogin").copied(),
        Some("modified"),
        "handleLogin body changed (added rate-limit check) — expected change_type=modified"
    );

    // ── (1b) isRateLimited is a brand-new function → must be `added`.
    assert!(
        names.contains(&"isRateLimited"),
        "expected isRateLimited (newly added function) in changed_symbols, got {names:?}"
    );
    assert_eq!(
        change_types.get("isRateLimited").copied(),
        Some("added"),
        "isRateLimited is brand new — expected change_type=added"
    );

    // ── (2) lookupUser and verifyPassword merely got shifted by the new
    // function's insertion above them. Their bodies didn't change, so they
    // MUST NOT appear in changed_symbols.
    assert!(
        !names.contains(&"lookupUser"),
        "lookupUser was only shifted in line number, not modified. names={names:?}"
    );
    assert!(
        !names.contains(&"verifyPassword"),
        "verifyPassword was only shifted, not modified. names={names:?}"
    );

    // ── (3) Process filter: virtual aggregates must never leak.
    for s in symbols {
        let kind = s["type"].as_str().unwrap_or("");
        assert_ne!(kind, "Process", "Process node leaked into changed_symbols");
        assert_ne!(kind, "File", "File node leaked into changed_symbols");
    }

    // ── risk bucket: handleLogin reaches dbQuery/hashPassword via process
    // traces ⇒ affected_count ≥ 1 ⇒ risk ∈ {medium, high, critical}.
    let summary = &result["summary"];
    let risk = summary["risk_level"].as_str().unwrap();
    assert_ne!(
        risk, "none",
        "risk should not be 'none' when there are changes"
    );

    // ── changed_files count = 1 (only auth.ts).
    assert_eq!(summary["changed_files"].as_u64().unwrap(), 1);
}

#[test]
fn detect_changes_default_output_is_folded_compact() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo_and_analyze(repo, home_tmp.path());

    std::fs::write(repo.join("src/auth.ts"), MODIFIED).unwrap();

    let output = run_gnx_text(repo, home_tmp.path(), &["detect-changes", "--repo", "."]);

    // Header: "risk <level>  files <n>; changed <m>; flows <f>"
    assert!(
        output.starts_with("risk "),
        "default output should be compact starting with 'risk '; got:\n{output}"
    );
    assert!(
        output.contains("files 1"),
        "header should mention `files 1`; got:\n{output}"
    );

    // Changed symbols section: `~ fn handleLogin:...`, `+ fn isRateLimited:...`
    assert!(
        output.contains("~ fn handleLogin"),
        "expected `~ fn handleLogin` in compact output; got:\n{output}"
    );
    assert!(
        output.contains("+ fn isRateLimited"),
        "expected `+ fn isRateLimited` in compact output; got:\n{output}"
    );

    // Phantom-match guard: bodies of lookupUser/verifyPassword unchanged →
    // must NOT appear (Plan B drops same-hash symbols even on shifted lines).
    assert!(
        !output.contains("lookupUser"),
        "lookupUser should be hash-equal → must not appear in compact; got:\n{output}"
    );
    assert!(
        !output.contains("verifyPassword"),
        "verifyPassword should be hash-equal → must not appear; got:\n{output}"
    );
}

#[test]
fn detect_changes_no_diff_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo_and_analyze(repo, home_tmp.path());

    // No modification → empty diff.
    let result = run_gnx_json(
        repo,
        home_tmp.path(),
        &["detect-changes", "--repo", ".", "--format", "json"],
    );

    let s = &result["summary"];
    assert_eq!(s["risk_level"].as_str().unwrap(), "none");
    assert_eq!(s["changed_count"].as_u64().unwrap(), 0);
    assert_eq!(s["affected_count"].as_u64().unwrap(), 0);

    assert!(result["changed_symbols"].as_array().unwrap().is_empty());
    assert!(result["affected_processes"].as_array().unwrap().is_empty());
}

#[test]
fn detect_changes_invalid_scope_errors_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo_and_analyze(repo, home_tmp.path());

    let out = Command::new(gnx_bin())
        .args([
            "detect-changes",
            "--repo",
            ".",
            "--scope",
            "compare",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    // `compare` without --base-ref should fail cleanly.
    assert!(
        !out.status.success(),
        "compare without base-ref should fail; got success with stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("base_ref") || stderr.contains("base-ref"),
        "expected base_ref error message, got: {stderr}"
    );
}
