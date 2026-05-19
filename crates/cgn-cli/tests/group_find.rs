//! Tests for `cgn group find`.
//!
//! Strategy:
//! - Smoke tests: `--help` exits 0 and unknown-group exits non-zero.
//! - Wiring test: 2-repo fixture indexed + grouped → JSON `per_repo` array
//!   with 2 entries, each having `repo`, `count`, and `hits` keys.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

fn run_cgn(args: &[&str], home: &Path) -> std::process::Output {
    Command::new(cgn_bin())
        .args(args)
        .env("HOME", home)
        .output()
        .expect("cgn spawn failed")
}

fn init_git_repo_with_rs(path: &Path) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(
        path.join("src/lib.rs"),
        "pub fn hello_world() -> &'static str { \"hello\" }\n",
    )
    .unwrap();
    fs::write(path.join("README.md"), "demo repo").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["-c", "user.email=t@t.t", "-c", "user.name=t", "add", "-A"],
        vec![
            "-c",
            "user.email=t@t.t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ],
    ] {
        Command::new("git")
            .current_dir(path)
            .args(&args)
            .status()
            .unwrap();
    }
}

fn read_dir_names(home_cgn: &Path) -> Vec<String> {
    let registry_path = home_cgn.join("registry.json");
    let bytes = fs::read(&registry_path).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["repos"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

// ── Smoke tests ───────────────────────────────────────────────────────────────

#[test]
fn group_find_help_exits_zero() {
    let out = Command::new(cgn_bin())
        .args(["group", "find", "--help"])
        .output()
        .expect("cgn spawn failed");
    assert!(
        out.status.success(),
        "expected exit 0 for --help; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("pattern") || stdout.contains("name"),
        "help text should mention pattern/name:\n{stdout}"
    );
}

#[test]
fn group_find_unknown_group_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_cgn(
        &["group", "find", "__no_such_group__", "foo"],
        tmp.path(),
    );
    assert!(
        !out.status.success(),
        "expected non-zero exit for unknown group"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("group"),
        "error should mention group: {stderr}"
    );
}

// ── Wiring / JSON-shape test ──────────────────────────────────────────────────

/// 2-repo fixture: JSON output must have `per_repo` with 2 entries, each
/// carrying `repo`, `count`, and `hits` keys.
#[test]
fn group_find_json_shape_two_repos() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    let home_cgn = home.join(".cgn");

    for name in ["repo_a", "repo_b"] {
        let repo = repos_tmp.path().join(name);
        init_git_repo_with_rs(&repo);
        let out = run_cgn(&["admin", "index", "--repo", repo.to_str().unwrap()], home);
        assert!(
            out.status.success(),
            "{name} admin index failed:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let dir_names = read_dir_names(&home_cgn);
    assert_eq!(dir_names.len(), 2, "expected 2 registered repos");

    for dn in &dir_names {
        let out = run_cgn(&["admin", "group", "add", dn, "findgrp"], home);
        assert!(
            out.status.success(),
            "admin group add failed for {dn}:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = run_cgn(&["group", "find", "findgrp", "hello", "--json"], home);
    assert!(
        out.status.success(),
        "group find failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");

    assert!(
        v.get("per_repo").is_some(),
        "`per_repo` key missing:\n{stdout}"
    );
    let per_repo = v["per_repo"].as_array().unwrap();
    assert_eq!(
        per_repo.len(),
        2,
        "expected 2 per_repo entries, got {}: {stdout}",
        per_repo.len()
    );
    for entry in per_repo {
        assert!(entry.get("repo").is_some(), "entry missing `repo`");
        assert!(entry.get("count").is_some(), "entry missing `count`");
        assert!(entry.get("hits").is_some(), "entry missing `hits`");
    }
}

// ── --merge rrf (consolidated from former `cgn group search`) ─────────────────

/// `--merge rrf` returns a unified top-K via Reciprocal Rank Fusion. JSON
/// shape: `{results: [...], per_repo: [{repo, count}, ...]}`.
#[test]
fn group_find_merge_rrf_json_shape_two_repos() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    let home_cgn = home.join(".cgn");

    for name in ["repo_a_rrf", "repo_b_rrf"] {
        let repo = repos_tmp.path().join(name);
        init_git_repo_with_rs(&repo);
        let out = run_cgn(&["admin", "index", "--repo", repo.to_str().unwrap()], home);
        assert!(
            out.status.success(),
            "{name} admin index failed:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let dir_names = read_dir_names(&home_cgn);
    for dn in &dir_names {
        let out = run_cgn(&["admin", "group", "add", dn, "rrfgrp"], home);
        assert!(
            out.status.success(),
            "admin group add failed for {dn}:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = run_cgn(
        &[
            "group", "find", "rrfgrp", "hello",
            "--merge", "rrf",
            "--limit", "3",
            "--json",
        ],
        home,
    );
    assert!(
        out.status.success(),
        "group find --merge rrf failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    assert!(v.get("results").is_some(), "`results` missing:\n{stdout}");
    assert!(v.get("per_repo").is_some(), "`per_repo` missing:\n{stdout}");
    let per_repo = v["per_repo"].as_array().unwrap();
    assert_eq!(per_repo.len(), 2, "expected 2 per_repo entries:\n{stdout}");
}

/// `--limit` without `--merge rrf` is rejected — per-repo concat has no
/// global top-K. Guards against silent semantic drift if a future change
/// drops the validation in `group::find::run`.
#[test]
fn group_find_limit_without_merge_rrf_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_cgn(
        &["group", "find", "anygrp", "hello", "--limit", "5"],
        tmp.path(),
    );
    assert!(
        !out.status.success(),
        "expected non-zero exit for --limit without --merge rrf"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--limit") && stderr.contains("merge rrf"),
        "stderr should mention the flag combo: {stderr}"
    );
}
