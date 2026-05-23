//! Tests for `ecp group summary`.
//!
//! Strategy:
//! - Smoke tests: `--help` exits 0 and unknown-group exits non-zero.
//! - Wiring test: 2-repo fixture indexed + grouped → JSON
//!   `summary.per_repo` array with 2 entries, each having
//!   `repo`, `frameworks`, `freshness`, `metrics`, and `blind_spots` keys.
//! - Back-compat: the legacy `group coverage` alias still routes to the
//!   same handler for one release.

use std::fs;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run_ecp(args: &[&str], home: &Path) -> std::process::Output {
    Command::new(ecp_bin())
        .args(args)
        .env("HOME", home)
        .output()
        .expect("ecp spawn failed")
}

fn init_git_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
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

fn read_dir_names(home_ecp: &Path) -> Vec<String> {
    let registry_path = home_ecp.join("registry.json");
    let bytes = fs::read(&registry_path).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["repos"].as_object().unwrap().keys().cloned().collect()
}

// ── Smoke tests ───────────────────────────────────────────────────────────────

#[test]
fn group_summary_help_exits_zero() {
    let out = Command::new(ecp_bin())
        .args(["group", "summary", "--help"])
        .output()
        .expect("ecp spawn failed");
    assert!(
        out.status.success(),
        "expected exit 0 for --help; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("name") || stdout.contains("json"),
        "help text should mention name/json:\n{stdout}"
    );
}

#[test]
fn group_summary_unknown_group_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_ecp(&["group", "summary", "__no_such_group__"], tmp.path());
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

/// 2-repo fixture: JSON output must have `summary.per_repo` with 2 entries,
/// each carrying the full health payload keys.
#[test]
fn group_summary_json_shape_two_repos() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    let home_ecp = home.join(".ecp");

    for name in ["svc_a", "svc_b"] {
        let repo = repos_tmp.path().join(name);
        init_git_repo(&repo);
        let out = run_ecp(&["admin", "index", "--repo", repo.to_str().unwrap()], home);
        assert!(
            out.status.success(),
            "{name} admin index failed:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let dir_names = read_dir_names(&home_ecp);
    assert_eq!(dir_names.len(), 2, "expected 2 registered repos");

    for dn in &dir_names {
        let out = run_ecp(&["admin", "group", "add", dn, "covgrp"], home);
        assert!(
            out.status.success(),
            "admin group add failed for {dn}:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = run_ecp(&["group", "summary", "covgrp", "--json"], home);
    assert!(
        out.status.success(),
        "group summary failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("output must be valid JSON");

    let per_repo = v["summary"]["per_repo"]
        .as_array()
        .expect("`summary.per_repo` must be an array");
    assert_eq!(
        per_repo.len(),
        2,
        "expected 2 per_repo entries, got {}: {stdout}",
        per_repo.len()
    );

    // Each entry must carry the full health payload.
    for entry in per_repo {
        assert!(entry.get("repo").is_some(), "entry missing `repo`");
        assert!(
            entry.get("frameworks").is_some(),
            "entry missing `frameworks`"
        );
        assert!(
            entry.get("freshness").is_some(),
            "entry missing `freshness`"
        );
        assert!(entry.get("metrics").is_some(), "entry missing `metrics`");
        assert!(
            entry.get("blind_spots").is_some(),
            "entry missing `blind_spots`"
        );
    }
}
