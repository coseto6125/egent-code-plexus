//! Integration tests for `ecp admin group add/remove`.
//!
//! Registry redirection: set HOME to a temp dir so `resolve_home_ecp`
//! resolves to `<temp>/.ecp/registry.json`.

use ecp_core::registry::{GroupEntry, RegistryFile, RepoAlias};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run_admin_group(home: &Path, args: &[&str]) -> std::process::Output {
    let mut full_args = vec!["admin", "group"];
    full_args.extend_from_slice(args);
    Command::new(ecp_bin())
        .args(&full_args)
        .env("HOME", home)
        .output()
        .expect("ecp spawn failed")
}

fn read_registry(home: &Path) -> RegistryFile {
    RegistryFile::read_or_empty(&home.join(".ecp/registry.json")).unwrap()
}

/// Seed `<home>/.ecp/registry.json` with the given repos and initial groups.
fn seed_registry(home: &Path, repos: Vec<RepoAlias>, groups: Vec<GroupEntry>) {
    let home_ecp = home.join(".ecp");
    std::fs::create_dir_all(&home_ecp).unwrap();
    let mut repo_map = BTreeMap::new();
    for r in repos {
        repo_map.insert(r.dir_name.clone(), r);
    }
    let reg = RegistryFile {
        version: 2,
        repos: repo_map,
        groups,
    };
    RegistryFile::write_atomic(&home_ecp.join("registry.json"), &reg).unwrap();
}

fn make_repo(name: &str, groups: Vec<String>) -> RepoAlias {
    RepoAlias {
        dir_name: format!("{name}__aabbccdd"),
        common_dir: format!("/tmp/{name}/.git"),
        remote_url: Some(format!("https://example.com/{name}.git")),
        aliases: vec![name.into()],
        last_touched: "2026-05-16T00:00:00Z".into(),
        groups,
    }
}

// ── help smoke tests ──────────────────────────────────────────────────────────

#[test]
fn group_help_works() {
    let out = Command::new(ecp_bin())
        .args(["admin", "group", "--help"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin group --help failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("add") || stdout.contains("Add"),
        "missing add in help"
    );
    assert!(
        stdout.contains("remove") || stdout.contains("Remove"),
        "missing remove in help"
    );
}

#[test]
fn group_add_help_works() {
    let out = Command::new(ecp_bin())
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
    let out = Command::new(ecp_bin())
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

    // group.rs matches by alias ("alpha") so we pass the alias, not dir_name.
    let out = run_admin_group(home.path(), &["add", "alpha", "newgroup"]);
    assert!(
        out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let reg = read_registry(home.path());
    let alias = reg
        .repos
        .values()
        .find(|r| r.aliases.iter().any(|a| a == "alpha"))
        .unwrap();
    assert_eq!(alias.groups, vec!["newgroup"]);
    assert_eq!(reg.groups.len(), 1, "expected 1 group entry");
    assert_eq!(reg.groups[0].name, "newgroup");
    // group member is whatever string the user passed (alias "alpha")
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
    let alias = reg
        .repos
        .values()
        .find(|r| r.aliases.iter().any(|a| a == "alpha"))
        .unwrap();
    assert_eq!(
        alias.groups.len(),
        1,
        "expected 1 group entry, got {:?}",
        alias.groups
    );
    let group = reg.groups.iter().find(|g| g.name == "backend").unwrap();
    assert_eq!(
        group.members.len(),
        1,
        "expected 1 member, got {:?}",
        group.members
    );
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
    let alias = reg
        .repos
        .values()
        .find(|r| r.aliases.iter().any(|a| a == "alpha"))
        .unwrap();
    assert!(
        alias.groups.is_empty(),
        "alias.groups should be empty after remove"
    );
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
            // members use the alias strings (whatever was passed to `ecp admin group add`)
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
    assert_eq!(
        reg.groups.len(),
        1,
        "group should still exist with 1 member"
    );
    let group = reg.groups.iter().find(|g| g.name == "backend").unwrap();
    assert_eq!(group.members, vec!["beta"]);
    let alpha = reg
        .repos
        .values()
        .find(|r| r.aliases.iter().any(|a| a == "alpha"))
        .unwrap();
    assert!(
        alpha.groups.is_empty(),
        "alpha.groups should be empty after remove"
    );
    let beta = reg
        .repos
        .values()
        .find(|r| r.aliases.iter().any(|a| a == "beta"))
        .unwrap();
    assert_eq!(
        beta.groups,
        vec!["backend"],
        "beta.groups should still have backend"
    );
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
