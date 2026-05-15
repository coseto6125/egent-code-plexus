//! Integration tests for `gnx admin group add/remove`.
//!
//! Registry redirection: set HOME to a temp dir so `resolve_home_gnx`
//! resolves to `<temp>/.gnx/registry.json`.

use graph_nexus_core::registry::{GroupEntry, RegistryFile, RepoEntry};
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_admin_group(home: &Path, args: &[&str]) -> std::process::Output {
    let mut full_args = vec!["admin", "group"];
    full_args.extend_from_slice(args);
    Command::new(gnx_bin())
        .args(&full_args)
        .env("HOME", home)
        .output()
        .expect("gnx spawn failed")
}

fn read_registry(home: &Path) -> RegistryFile {
    RegistryFile::read_or_empty(&home.join(".gnx/registry.json")).unwrap()
}

/// Seed `<home>/.gnx/registry.json` with the given repos and initial groups.
fn seed_registry(home: &Path, repos: Vec<RepoEntry>, groups: Vec<GroupEntry>) {
    let home_gnx = home.join(".gnx");
    std::fs::create_dir_all(&home_gnx).unwrap();
    let reg = RegistryFile {
        version: 1,
        repos,
        groups,
    };
    RegistryFile::write_atomic(&home_gnx.join("registry.json"), &reg).unwrap();
}

fn make_repo(name: &str, groups: Vec<String>) -> RepoEntry {
    RepoEntry {
        name: name.into(),
        remote_url: format!("https://example.com/{name}.git"),
        worktree_path: format!("/tmp/{name}"),
        index_dir_root: format!("/tmp/idx/{name}"),
        branches: vec![],
        groups,
    }
}

// ── help smoke tests ──────────────────────────────────────────────────────────

#[test]
fn group_help_works() {
    let out = Command::new(gnx_bin())
        .args(["admin", "group", "--help"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin group --help failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("add") || stdout.contains("Add"), "missing add in help");
    assert!(
        stdout.contains("remove") || stdout.contains("Remove"),
        "missing remove in help"
    );
}

#[test]
fn group_add_help_works() {
    let out = Command::new(gnx_bin())
        .args(["admin", "group", "add", "--help"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin group add --help failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn group_remove_help_works() {
    let out = Command::new(gnx_bin())
        .args(["admin", "group", "remove", "--help"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin group remove --help failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── round-trip tests ──────────────────────────────────────────────────────────

#[test]
fn group_add_creates_group_if_missing() {
    let home = tempfile::tempdir().unwrap();
    seed_registry(home.path(), vec![make_repo("alpha", vec![])], vec![]);

    let out = run_admin_group(home.path(), &["add", "alpha", "newgroup"]);
    assert!(
        out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let reg = read_registry(home.path());
    let repo = reg.repos.iter().find(|r| r.name == "alpha").unwrap();
    assert_eq!(repo.groups, vec!["newgroup"]);
    assert_eq!(reg.groups.len(), 1, "expected 1 group entry");
    assert_eq!(reg.groups[0].name, "newgroup");
    assert_eq!(reg.groups[0].members, vec!["alpha"]);
}

#[test]
fn group_add_idempotent() {
    let home = tempfile::tempdir().unwrap();
    seed_registry(home.path(), vec![make_repo("alpha", vec![])], vec![]);

    let _ = run_admin_group(home.path(), &["add", "alpha", "backend"]);
    let out = run_admin_group(home.path(), &["add", "alpha", "backend"]);
    assert!(
        out.status.success(),
        "second add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let reg = read_registry(home.path());
    let repo = reg.repos.iter().find(|r| r.name == "alpha").unwrap();
    assert_eq!(repo.groups.len(), 1, "expected 1 group entry, got {:?}", repo.groups);
    let group = reg.groups.iter().find(|g| g.name == "backend").unwrap();
    assert_eq!(group.members.len(), 1, "expected 1 member, got {:?}", group.members);
}

#[test]
fn group_add_unknown_repo_fails() {
    let home = tempfile::tempdir().unwrap();
    seed_registry(home.path(), vec![], vec![]);

    let out = run_admin_group(home.path(), &["add", "no-such-repo", "mygroup"]);
    assert!(
        !out.status.success(),
        "expected failure for unknown repo; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("no-such-repo"),
        "expected error mentioning missing repo; stderr={stderr}"
    );
}

#[test]
fn group_remove_auto_deletes_empty_group() {
    let home = tempfile::tempdir().unwrap();
    seed_registry(home.path(), vec![make_repo("alpha", vec![])], vec![]);
    let _ = run_admin_group(home.path(), &["add", "alpha", "solo"]);

    let out = run_admin_group(home.path(), &["remove", "alpha", "solo"]);
    assert!(
        out.status.success(),
        "remove failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let reg = read_registry(home.path());
    assert!(
        reg.groups.is_empty(),
        "expected empty groups after removing last member, got {:?}",
        reg.groups
    );
    let repo = reg.repos.iter().find(|r| r.name == "alpha").unwrap();
    assert!(repo.groups.is_empty(), "repo.groups should be empty after remove");
}

#[test]
fn group_remove_preserves_non_empty_group() {
    let home = tempfile::tempdir().unwrap();
    seed_registry(
        home.path(),
        vec![
            make_repo("alpha", vec!["backend".into()]),
            make_repo("beta", vec!["backend".into()]),
        ],
        vec![GroupEntry {
            name: "backend".into(),
            members: vec!["alpha".into(), "beta".into()],
        }],
    );

    let out = run_admin_group(home.path(), &["remove", "alpha", "backend"]);
    assert!(
        out.status.success(),
        "remove failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let reg = read_registry(home.path());
    assert_eq!(reg.groups.len(), 1, "group should still exist with 1 member");
    let group = reg.groups.iter().find(|g| g.name == "backend").unwrap();
    assert_eq!(group.members, vec!["beta"]);
    let alpha = reg.repos.iter().find(|r| r.name == "alpha").unwrap();
    assert!(alpha.groups.is_empty(), "alpha.groups should be empty after remove");
    let beta = reg.repos.iter().find(|r| r.name == "beta").unwrap();
    assert_eq!(beta.groups, vec!["backend"], "beta.groups should still have backend");
}

#[test]
fn group_remove_noop_when_not_a_member() {
    let home = tempfile::tempdir().unwrap();
    seed_registry(home.path(), vec![make_repo("alpha", vec![])], vec![]);

    let out = run_admin_group(home.path(), &["remove", "alpha", "nonexistent-group"]);
    assert!(
        out.status.success(),
        "remove no-op should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
