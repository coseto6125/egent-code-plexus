//! Regression tests: top-level commands reject `--repo @<group>` and emit
//! a hint pointing at `gnx group <verb>`. `@all` must continue to work.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn make_home_with_group(home: &std::path::Path, group_name: &str) {
    let reg = serde_json::json!({
        "version": 2,
        "repos": {},
        "groups": [{"name": group_name, "members": []}]
    });
    fs::create_dir_all(home.join(".gnx")).unwrap();
    fs::write(
        home.join(".gnx/registry.json"),
        serde_json::to_vec_pretty(&reg).unwrap(),
    )
    .unwrap();
}

#[test]
fn search_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["find", "--mode", "bm25", "--repo", "@demo", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("gnx group find"),
        "missing hint; got: {stderr}"
    );
}

#[test]
fn contracts_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["contracts", "--repo", "@demo"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gnx group contracts"), "got: {stderr}");
}

#[test]
fn find_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["find", "--repo", "@demo", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gnx group find"), "got: {stderr}");
}

#[test]
fn coverage_at_group_errors_with_hint() {
    let home = TempDir::new().unwrap();
    make_home_with_group(home.path(), "demo");
    let out = Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["coverage", "--repo", "@demo"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gnx group coverage"), "got: {stderr}");
}

#[test]
fn search_at_all_still_works_on_empty_registry() {
    // @all on an empty registry should succeed with no results, not error.
    let home = TempDir::new().unwrap();
    let reg = serde_json::json!({"version": 2, "repos": {}, "groups": []});
    fs::create_dir_all(home.path().join(".gnx")).unwrap();
    fs::write(
        home.path().join(".gnx/registry.json"),
        serde_json::to_vec_pretty(&reg).unwrap(),
    )
    .unwrap();
    let out = Command::new(gnx_bin())
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
        !combined.contains("gnx group"),
        "@all should not suggest gnx group: {combined}"
    );
}
