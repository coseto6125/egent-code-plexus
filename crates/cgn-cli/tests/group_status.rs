//! Integration tests for `cgn group status`.

use cgn_cli::commands::group::storage::{group_dir, write_meta, GroupMeta, RepoSnapshot};
use std::collections::BTreeMap;
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
        .expect("cgn spawn failed")
}

fn init_git_repo(path: &Path) -> String {
    fs::create_dir_all(path).unwrap();
    fs::write(path.join("README.md"), "x").unwrap();
    Command::new("git")
        .current_dir(path)
        .args(["init", "-q"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(path)
        .args(["-c", "user.email=t@t.t", "-c", "user.name=t", "add", "-A"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(path)
        .args(["-c", "user.email=t@t.t", "-c", "user.name=t", "commit", "-qm", "init"])
        .status()
        .unwrap();
    let out = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn read_first_dir_name(home_gnx: &Path) -> String {
    let registry_path = home_gnx.join("registry.json");
    let bytes = fs::read(&registry_path).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["repos"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone()
}

#[test]
fn status_never_synced_reports_no_meta() {
    let home_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    let home_gnx = home.join(".gnx");

    let repos_tmp = tempfile::tempdir().unwrap();
    let repo = repos_tmp.path().join("backend");
    init_git_repo(&repo);

    let out = run_gnx(&["admin", "index", "--repo", repo.to_str().unwrap()], home);
    assert!(
        out.status.success(),
        "admin index failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let dir_name = read_first_dir_name(&home_gnx);

    // admin group add <repo> <group>
    let out = run_gnx(&["admin", "group", "add", &dir_name, "demo"], home);
    assert!(
        out.status.success(),
        "admin group add failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    let out = run_gnx(&["group", "status", "demo"], home);
    assert!(
        out.status.success(),
        "group status failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("never synced"), "expected 'never synced'; got: {s}");
    assert!(s.contains("NO_META"), "expected 'NO_META'; got: {s}");
}

#[test]
fn status_reports_stale_when_meta_commit_differs_from_head() {
    let home_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    let home_gnx = home.join(".gnx");

    let repos_tmp = tempfile::tempdir().unwrap();
    let repo = repos_tmp.path().join("backend");
    init_git_repo(&repo);

    let out = run_gnx(&["admin", "index", "--repo", repo.to_str().unwrap()], home);
    assert!(
        out.status.success(),
        "admin index failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let dir_name = read_first_dir_name(&home_gnx);

    let out = run_gnx(&["admin", "group", "add", &dir_name, "demo"], home);
    assert!(
        out.status.success(),
        "admin group add failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    // Write a meta.json with a deliberately-wrong stored commit.
    let gdir = group_dir(&home_gnx, "demo");
    let mut snapshots = BTreeMap::new();
    snapshots.insert(
        dir_name.clone(),
        RepoSnapshot {
            indexed_at: "2026-05-18T10:00:00Z".into(),
            last_commit: "0000000000000000000000000000000000000000".into(),
        },
    );
    write_meta(
        &gdir,
        &GroupMeta {
            version: 1,
            generated_at: "2026-05-18T10:05:00Z".into(),
            repo_snapshots: snapshots,
            missing_repos: vec![],
            config_source: "default".into(),
        },
    )
    .unwrap();

    let out = run_gnx(&["group", "status", "demo"], home);
    assert!(
        out.status.success(),
        "group status failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("STALE"), "expected STALE marker; got: {s}");
}
