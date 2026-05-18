//! Tests for `gnx group search`.
//!
//! Strategy:
//! - Smoke tests: `--help` exits 0 and unknown-group exits non-zero.
//! - Wiring test: 2-repo fixture indexed + grouped → JSON output has `results`
//!   and `per_repo` keys (merged mode) or `per_repo` key (--no-merge mode).

use std::fs;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_gnx(args: &[&str], home: &Path) -> std::process::Output {
    Command::new(gnx_bin())
        .args(args)
        .env("HOME", home)
        .output()
        .expect("gnx spawn failed")
}

fn init_git_repo_with_rs(path: &Path) {
    fs::create_dir_all(path.join("src")).unwrap();
    // Minimal Rust source so BM25 / substring scan has a symbol to find.
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

fn read_dir_names(home_gnx: &Path) -> Vec<String> {
    let registry_path = home_gnx.join("registry.json");
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
fn group_search_help_exits_zero() {
    let out = Command::new(gnx_bin())
        .args(["group", "search", "--help"])
        .output()
        .expect("gnx spawn failed");
    assert!(
        out.status.success(),
        "expected exit 0 for --help; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("query") || stdout.contains("limit"),
        "help text should mention query/limit:\n{stdout}"
    );
}

#[test]
fn group_search_unknown_group_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_gnx(
        &["group", "search", "__no_such_group__", "hello"],
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

// ── Wiring / JSON-shape tests ─────────────────────────────────────────────────

/// Set up 2 repos, index them, add to a group, run `gnx group search --json`
/// and verify the merged output shape: `results` + `per_repo` keys.
#[test]
fn group_search_merged_json_shape() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    let home_gnx = home.join(".gnx");

    // Create and index 2 repos.
    for name in ["alpha", "beta"] {
        let repo = repos_tmp.path().join(name);
        init_git_repo_with_rs(&repo);
        let out = run_gnx(&["admin", "index", "--repo", repo.to_str().unwrap()], home);
        assert!(
            out.status.success(),
            "{name} admin index failed:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Both repos are now in the registry; read their dir_names.
    let dir_names = read_dir_names(&home_gnx);
    assert_eq!(dir_names.len(), 2, "expected 2 registered repos");

    // Add both to group "mygrp".
    for dn in &dir_names {
        let out = run_gnx(&["admin", "group", "add", dn, "mygrp"], home);
        assert!(
            out.status.success(),
            "admin group add failed for {dn}:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Run group search (merged, JSON).
    let out = run_gnx(&["group", "search", "mygrp", "hello", "--json"], home);
    assert!(
        out.status.success(),
        "group search failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");

    // JSON shape must have both keys.
    assert!(
        v.get("results").is_some(),
        "`results` key missing in merged output:\n{stdout}"
    );
    assert!(
        v.get("per_repo").is_some(),
        "`per_repo` key missing in merged output:\n{stdout}"
    );

    // per_repo should reference 2 repos.
    let per_repo = v["per_repo"].as_array().unwrap();
    assert_eq!(
        per_repo.len(),
        2,
        "expected 2 per_repo entries, got {}: {stdout}",
        per_repo.len()
    );
}

/// `--no-merge` mode: JSON must have `per_repo` key with per-repo hit arrays.
#[test]
fn group_search_no_merge_json_shape() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    let home_gnx = home.join(".gnx");

    for name in ["gamma", "delta"] {
        let repo = repos_tmp.path().join(name);
        init_git_repo_with_rs(&repo);
        let out = run_gnx(&["admin", "index", "--repo", repo.to_str().unwrap()], home);
        assert!(
            out.status.success(),
            "{name} admin index failed:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let dir_names = read_dir_names(&home_gnx);
    for dn in &dir_names {
        let out = run_gnx(&["admin", "group", "add", dn, "grp2"], home);
        assert!(out.status.success(), "admin group add failed for {dn}");
    }

    let out = run_gnx(
        &["group", "search", "grp2", "hello", "--no-merge", "--json"],
        home,
    );
    assert!(
        out.status.success(),
        "group search --no-merge failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");

    assert!(
        v.get("per_repo").is_some(),
        "`per_repo` key missing in --no-merge output:\n{stdout}"
    );
    let per_repo = v["per_repo"].as_array().unwrap();
    assert_eq!(
        per_repo.len(),
        2,
        "expected 2 per_repo entries:\n{stdout}"
    );
    // Each entry must have `repo` and `hits` keys.
    for entry in per_repo {
        assert!(entry.get("repo").is_some(), "missing `repo` in entry");
        assert!(entry.get("hits").is_some(), "missing `hits` in entry");
    }
}
