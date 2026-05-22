//! Regression tests: top-level commands reject `--repo @<group>` and emit
//! a hint pointing at `ecp group <verb>`. `@all` must continue to work.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn make_home_with_group(home: &std::path::Path, group_name: &str) {
    let reg = serde_json::json!({
        "version": 2,
        "repos": {},
        "groups": [{"name": group_name, "members": []}]
    });
    fs::create_dir_all(home.join(".ecp")).unwrap();
    fs::write(
        home.join(".ecp/registry.json"),
        serde_json::to_vec_pretty(&reg).unwrap(),
    )
    .unwrap();
}

#[test]
fn search_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["find", "--mode", "bm25", "--repo", "@demo", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ecp group find"),
        "missing hint; got: {stderr}"
    );
}

#[test]
fn contracts_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["contracts", "--repo", "@demo"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ecp group contracts"), "got: {stderr}");
}

#[test]
fn find_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["find", "--repo", "@demo", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ecp group find"), "got: {stderr}");
}

#[test]
fn summary_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["summary", "--repo", "@demo"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ecp group summary"), "got: {stderr}");
}

// ── Commands with a direct `ecp group <verb>` analog ───────────────────────

#[test]
fn impact_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["impact", "x", "--repo", "@demo"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ecp group impact"), "got: {stderr}");
}

// ── Commands without a group analog → redirect to `ecp group --help` ────────

fn assert_redirects_to_group_help(args: &[&str]) {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(args)
        .output()
        .unwrap();
    assert!(!out.status.success(), "args={args:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ecp group --help"),
        "args={args:?}, stderr={stderr}"
    );
    // Sanity: should NOT silently expand to multi-repo or hit auto_ensure
    // path-not-found.
    assert!(
        !stderr.contains("resolved to") && !stderr.contains("Error preparing index"),
        "leaked through guard: args={args:?}, stderr={stderr}"
    );
}

#[test]
fn inspect_at_group_redirects_to_group_help() {
    assert_redirects_to_group_help(&["inspect", "--name", "x", "--repo", "@demo"]);
}

#[test]
fn rename_at_group_redirects_to_group_help() {
    assert_redirects_to_group_help(&[
        "rename",
        "--symbol",
        "old",
        "--new-name",
        "new",
        "--repo",
        "@demo",
    ]);
}

#[test]
fn cypher_at_group_redirects_to_group_help() {
    assert_redirects_to_group_help(&["cypher", "MATCH (n) RETURN n", "--repo", "@demo"]);
}

#[test]
fn routes_at_group_redirects_to_group_help() {
    assert_redirects_to_group_help(&["routes", "--repo", "@demo"]);
}

#[test]
fn shape_check_at_group_redirects_to_group_help() {
    assert_redirects_to_group_help(&["shape-check", "--repo", "@demo"]);
}

#[test]
fn tool_map_at_group_redirects_to_group_help() {
    assert_redirects_to_group_help(&["tool-map", "--repo", "@demo"]);
}

#[test]
fn review_at_group_redirects_to_group_help() {
    assert_redirects_to_group_help(&["review", "--repo", "@demo"]);
}

#[test]
fn diff_at_group_redirects_to_group_help() {
    assert_redirects_to_group_help(&[
        "diff",
        "--section",
        "bindings",
        "--baseline",
        "HEAD~1",
        "--repo",
        "@demo",
    ]);
}

#[test]
fn search_at_all_still_works_on_empty_registry() {
    // @all on an empty registry should succeed with no results, not error.
    let home = TempDir::new().unwrap();
    let reg = serde_json::json!({"version": 2, "repos": {}, "groups": []});
    fs::create_dir_all(home.path().join(".ecp")).unwrap();
    fs::write(
        home.path().join(".ecp/registry.json"),
        serde_json::to_vec_pretty(&reg).unwrap(),
    )
    .unwrap();
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["find", "--mode", "bm25", "--repo", "@all", "x"])
        .output()
        .unwrap();
    // Either success-with-empty-results OR a clean "no repos" message —
    // NOT a group-related error.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("ecp group"),
        "@all should not suggest ecp group: {combined}"
    );
}
