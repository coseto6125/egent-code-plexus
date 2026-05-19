//! Tests for resolving (repo_name, branch, worktree_path) from cwd's git.

use cgn_cli::git_state::{resolve, GitState};
use std::path::Path;
use std::process::Command;

fn init_repo(path: &Path) {
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/test-repo.git",
        ])
        .current_dir(path)
        .output()
        .unwrap();
    std::fs::write(path.join("README.md"), "x").unwrap();
    Command::new("git")
        .args(["add", "README.md"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(path)
        .output()
        .unwrap();
}

#[test]
fn resolves_repo_name_branch_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let state: GitState = resolve(tmp.path()).unwrap();
    assert_eq!(state.repo_name, "test-repo");
    assert_eq!(state.branch, "main");
    let canonical = tmp.path().canonicalize().unwrap();
    assert_eq!(state.worktree_path, canonical);
    assert_eq!(
        state.remote_url.as_deref(),
        Some("git@github.com:E-NoR/test-repo.git")
    );
}

#[test]
fn falls_back_to_basename_if_no_remote() {
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    std::fs::write(tmp.path().join("x"), "x").unwrap();
    Command::new("git")
        .args(["add", "x"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "i",
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let state = resolve(tmp.path()).unwrap();
    assert!(!state.repo_name.is_empty());
    assert_eq!(state.branch, "main");
    assert_eq!(state.remote_url, None);
}

#[test]
fn errors_on_non_git_dir() {
    let tmp = tempfile::tempdir().unwrap();
    // No git init — should error
    let r = resolve(tmp.path());
    assert!(r.is_err());
}
