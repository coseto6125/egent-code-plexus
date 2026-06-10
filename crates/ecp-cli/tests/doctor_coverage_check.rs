//! `admin doctor coverage` — detects a graph that is partial at its own SHA.
//!
//! Behind-HEAD staleness is caught by the index check + query caveat, but a
//! build that silently dropped whole directories (observed in the wild: a
//! 0.39s build that indexed 1 of 4 workspace crates and then answered
//! `found:false, status:success` for symbols in the other three) looks fresh
//! to every sha/mtime probe. Coverage compares tracked files whose extension
//! the graph itself indexes elsewhere against the graph's File set — the
//! extension universe is self-calibrating, so unsupported languages never
//! false-positive.

use std::fs;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run_ecp(args: &[&str], home: &Path, cwd: &Path) -> std::process::Output {
    Command::new(ecp_bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .expect("ecp spawn failed")
}

fn git(repo: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(repo)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed in {}", repo.display());
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), "pub fn seed_fn() {}\n").unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "init"]);
}

fn coverage_result(out: &std::process::Output) -> serde_json::Value {
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "non-JSON doctor output ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    body["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|c| c["name"] == "coverage")
        .expect("coverage check present")
        .clone()
}

#[test]
fn fresh_full_graph_passes_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    init_repo(&repo);

    let out = run_ecp(&["admin", "index", "--repo", "."], &home, &repo);
    assert!(out.status.success());

    let out = run_ecp(
        &["admin", "doctor", "coverage", "--format", "json"],
        &home,
        &repo,
    );
    let check = coverage_result(&out);
    assert_eq!(check["status"], "ok", "full graph must pass: {check}");
}

#[test]
fn graph_missing_many_known_extension_files_fails_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    init_repo(&repo);

    let out = run_ecp(&["admin", "index", "--repo", "."], &home, &repo);
    assert!(out.status.success());

    // Simulate the partial-graph incident: many tracked .rs files the graph
    // (built before they existed) has no File node for.
    fs::create_dir_all(repo.join("core/src")).unwrap();
    for i in 0..12 {
        fs::write(
            repo.join(format!("core/src/mod_{i}.rs")),
            format!("pub fn core_fn_{i}() {{}}\n"),
        )
        .unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "add core crate"]);

    let out = run_ecp(
        &["admin", "doctor", "coverage", "--format", "json"],
        &home,
        &repo,
    );
    let check = coverage_result(&out);
    assert_eq!(
        check["status"], "fail",
        "12/13 .rs files absent from the graph must fail coverage: {check}"
    );
    let remediation = check["remediation"].as_str().unwrap_or_default();
    assert!(
        remediation.contains("--force"),
        "same-sha partial graphs need a force reindex, got: {remediation}"
    );
}

#[test]
fn coverage_fix_force_reindexes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    init_repo(&repo);

    let out = run_ecp(&["admin", "index", "--repo", "."], &home, &repo);
    assert!(out.status.success());

    fs::create_dir_all(repo.join("core/src")).unwrap();
    for i in 0..12 {
        fs::write(
            repo.join(format!("core/src/mod_{i}.rs")),
            format!("pub fn core_fn_{i}() {{}}\n"),
        )
        .unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "add core crate"]);

    let out = run_ecp(
        &["admin", "doctor", "coverage", "--fix", "--format", "json"],
        &home,
        &repo,
    );
    let check = coverage_result(&out);
    assert_eq!(check["fix_applied"], true, "fix must run: {check}");

    let out = run_ecp(
        &["admin", "doctor", "coverage", "--format", "json"],
        &home,
        &repo,
    );
    let check = coverage_result(&out);
    assert_eq!(check["status"], "ok", "post-fix graph must pass: {check}");
}
