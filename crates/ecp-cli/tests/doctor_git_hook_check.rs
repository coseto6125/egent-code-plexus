//! `admin doctor config` warns when the repository still holds the git
//! `reference-transaction` hook an older `ecp admin install-hook` wrote, and
//! names the command that removes it. Without such a hook it adds no row.

use std::path::{Path, PathBuf};
use std::process::Command;

const ECP_HOOK: &str =
    "#!/bin/sh\n# ecp-managed reference-transaction hook\nexec \"/usr/local/bin/ecp\" hook-handle \"$@\"\n";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo_and_home() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("my repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    (tmp, repo, home)
}

/// The `config:git-hook` rows of `ecp admin doctor config --format json`.
fn git_hook_rows(cwd: &Path, home: &Path) -> Vec<serde_json::Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["admin", "doctor", "config", "--format", "json"])
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        // Every fixture sits beside `home`, so its parent bounds git's search
        // and a temp dir that happens to live inside a repository stays out.
        .env("GIT_CEILING_DIRECTORIES", home.parent().unwrap())
        .output()
        .expect("ecp spawn failed");
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "non-JSON doctor output ({e}): {} / {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    body["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .filter(|c| c["name"] == "config:git-hook")
        .cloned()
        .collect()
}

fn write_hook(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn assert_warns_about(rows: &[serde_json::Value], hook: &Path) {
    let row = rows
        .iter()
        .find(|r| {
            r["message"]
                .as_str()
                .is_some_and(|m| m.contains(&*hook.to_string_lossy()))
        })
        .unwrap_or_else(|| panic!("no row names {}: {rows:?}", hook.display()));
    assert_eq!(row["status"], "warn", "{row}");
    assert_eq!(row["remediation"], "ecp admin uninstall-hook", "{row}");
}

#[test]
fn test_check_ecp_hook_in_default_hooks_dir_warns_with_removal_command() {
    let (_tmp, repo, home) = repo_and_home();
    let hook = repo.join(".git/hooks/reference-transaction");
    write_hook(&hook, ECP_HOOK);
    let rows = git_hook_rows(&repo, &home);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_warns_about(&rows, &hook);
}

#[test]
fn test_check_ecp_hook_in_relative_core_hooks_path_warns() {
    let (_tmp, repo, home) = repo_and_home();
    git(&repo, &["config", "core.hooksPath", "my hooks"]);
    let hook = repo.join("my hooks/reference-transaction");
    write_hook(&hook, ECP_HOOK);
    let sub = repo.join("sub dir");
    std::fs::create_dir_all(&sub).unwrap();
    let rows = git_hook_rows(&sub, &home);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_warns_about(&rows, &hook);
}

#[test]
fn test_check_ecp_hook_seen_from_linked_worktree_warns() {
    let (tmp, repo, home) = repo_and_home();
    let hook = repo.join(".git/hooks/reference-transaction");
    write_hook(&hook, ECP_HOOK);
    let linked = tmp.path().join("linked wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    let rows = git_hook_rows(&linked, &home);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_warns_about(&rows, &hook);
}

#[test]
fn test_check_no_hook_or_foreign_hook_adds_no_row() {
    let (_tmp, repo, home) = repo_and_home();
    assert!(git_hook_rows(&repo, &home).is_empty());

    write_hook(
        &repo.join(".git/hooks/reference-transaction"),
        "#!/bin/sh\nother-tool hook-handle \"$@\"\n",
    );
    assert!(git_hook_rows(&repo, &home).is_empty());
}

#[test]
fn test_check_outside_a_repository_adds_no_row() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&plain).unwrap();
    assert!(git_hook_rows(&plain, &home).is_empty());
}
