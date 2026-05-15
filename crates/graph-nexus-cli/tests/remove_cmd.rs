//! Tests `gnx remove <target>` deletes a registry entry + its index folder.
//!
//! Setup pattern mirrors `prune_cmd.rs`: spin up a temp HOME with a
//! pre-populated `.gnx/registry.json` (so we don't depend on a real git
//! repo or `analyze` running first), then invoke the CLI binary and assert
//! on filesystem + registry state. The binary subcommand is wired by the
//! orchestrator SA; these tests will exercise that wire-up.

use graph_nexus_core::registry::{BranchEntry, Registry, RegistryFile, RepoEntry};
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

/// Seed `<home>/.gnx/registry.json` with one repo + a fake index dir at
/// `<home>/.gnx/<name>/main/graph.bin`. Returns the index dir path so the
/// caller can later assert it was removed.
fn seed_repo(home: &Path, name: &str, worktree: &str) -> std::path::PathBuf {
    let home_gnx = home.join(".gnx");
    let index_dir = home_gnx.join(name).join("main");
    std::fs::create_dir_all(&index_dir).unwrap();
    std::fs::write(index_dir.join("graph.bin"), b"junk").unwrap();

    let mut reg = Registry::open(&home_gnx).unwrap();
    reg.upsert_repo(RepoEntry {
        name: name.into(),
        remote_url: "git@github.com:E-NoR/remove-test.git".into(),
        worktree_path: worktree.into(),
        index_dir_root: home_gnx.join(name).to_string_lossy().into(),
        branches: vec![BranchEntry {
            name: "main".into(),
            index_dir: index_dir.to_string_lossy().into(),
            indexed_at: "2026-05-14T00:00:00Z".into(),
            node_count: 1,
            delta_size: 0,
            embedding_status: "skipped".into(),
        }],
        group: None,
    })
    .unwrap();

    home_gnx.join(name)
}

fn read_registry(home: &Path) -> RegistryFile {
    RegistryFile::read_or_empty(&home.join(".gnx/registry.json")).unwrap()
}

#[test]
fn remove_by_name_deletes_registry_entry_and_folder() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_dir = seed_repo(home_tmp.path(), "alpha", "/w/alpha");
    assert!(repo_dir.exists(), "setup: index dir should exist");

    let out = Command::new(gnx_bin())
        .args(["remove", "alpha"])
        .env("HOME", home_tmp.path())
        .output()
        .expect("gnx spawn failed");

    assert!(
        out.status.success(),
        "remove failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!repo_dir.exists(), "expected {repo_dir:?} to be removed");
    let reg = read_registry(home_tmp.path());
    assert!(
        !reg.repos.iter().any(|r| r.name == "alpha"),
        "registry still contains 'alpha': {:?}",
        reg.repos.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
}

#[test]
fn remove_unknown_target_errors_with_helpful_message() {
    let home_tmp = tempfile::tempdir().unwrap();
    // No seed — registry stays empty.

    let out = Command::new(gnx_bin())
        .args(["remove", "no-such-thing"])
        .env("HOME", home_tmp.path())
        .output()
        .expect("gnx spawn failed");

    assert!(
        !out.status.success(),
        "expected non-zero exit for unknown target; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no registered repo"),
        "stderr should mention 'no registered repo'; got: {stderr}"
    );
}

#[test]
fn remove_by_path_match_finds_entry() {
    let home_tmp = tempfile::tempdir().unwrap();
    // Path-match branch requires `Path::is_absolute()` to be true; "/tmp/foo"
    // is absolute on Unix but not on Windows, so pick a platform-appropriate
    // root that Windows recognises (e.g. "C:\\tmp\\foo").
    let worktree = if cfg!(windows) {
        r"C:\tmp\foo"
    } else {
        "/tmp/foo"
    };
    let repo_dir = seed_repo(home_tmp.path(), "beta", worktree);
    assert!(repo_dir.exists());

    let out = Command::new(gnx_bin())
        .args(["remove", worktree])
        .env("HOME", home_tmp.path())
        .output()
        .expect("gnx spawn failed");

    assert!(
        out.status.success(),
        "remove by path failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!repo_dir.exists(), "expected index dir to be removed");
    let reg = read_registry(home_tmp.path());
    assert!(
        !reg.repos.iter().any(|r| r.name == "beta"),
        "registry still contains 'beta'"
    );
}
