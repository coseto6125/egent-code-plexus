//! `git_cache` answers HEAD / common-dir questions from `.git` files without
//! spawning `git` on the hot path. Every query paid two `git rev-parse`
//! spawns (~1 ms each) before this; the file readers must agree with git
//! byte-for-byte on every layout ecp meets, and return `None` (so the caller
//! falls back to the spawn) on anything they do not model.

mod common;

use common::{commit_all, run_git};
use ecp_cli::git_cache;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git failed to spawn");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn git_common_dir(repo: &Path) -> PathBuf {
    let rel = git_stdout(repo, &["rev-parse", "--git-common-dir"]);
    std::fs::canonicalize(repo.join(rel)).unwrap()
}

fn init_repo_with_commit() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    run_git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
    commit_all(tmp.path(), "first");
    tmp
}

#[test]
fn test_head_sha_from_files_attached_branch_matches_git() {
    let tmp = init_repo_with_commit();
    let expected = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    assert_eq!(git_cache::head_sha_from_files(tmp.path()), Some(expected));
}

#[test]
fn test_head_sha_from_files_packed_ref_matches_git() {
    let tmp = init_repo_with_commit();
    run_git(tmp.path(), &["pack-refs", "--all"]);
    assert!(
        !tmp.path().join(".git/refs/heads/main").exists(),
        "fixture: the loose ref must be gone so the packed-refs path is exercised"
    );
    let expected = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    assert_eq!(git_cache::head_sha_from_files(tmp.path()), Some(expected));
}

#[test]
fn test_head_sha_from_files_loose_ref_wins_over_stale_packed_ref() {
    let tmp = init_repo_with_commit();
    run_git(tmp.path(), &["pack-refs", "--all"]);
    std::fs::write(tmp.path().join("b.txt"), "y").unwrap();
    commit_all(tmp.path(), "second");
    let expected = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    assert_eq!(git_cache::head_sha_from_files(tmp.path()), Some(expected));
}

#[test]
fn test_head_sha_from_files_detached_head_matches_git() {
    let tmp = init_repo_with_commit();
    let first = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(tmp.path().join("b.txt"), "y").unwrap();
    commit_all(tmp.path(), "second");
    run_git(tmp.path(), &["checkout", "-q", "--detach", &first]);
    assert_eq!(git_cache::head_sha_from_files(tmp.path()), Some(first));
}

#[test]
fn test_head_sha_from_files_subdirectory_matches_git() {
    let tmp = init_repo_with_commit();
    let sub = tmp.path().join("src/deep");
    std::fs::create_dir_all(&sub).unwrap();
    let expected = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    assert_eq!(git_cache::head_sha_from_files(&sub), Some(expected));
}

#[test]
fn test_head_sha_from_files_linked_worktree_matches_git() {
    let tmp = init_repo_with_commit();
    let wt = tmp.path().join("wt");
    run_git(
        tmp.path(),
        &["worktree", "add", "-q", "-b", "topic", wt.to_str().unwrap()],
    );
    std::fs::write(wt.join("c.txt"), "z").unwrap();
    commit_all(&wt, "on topic");
    let main_sha = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    let topic_sha = git_stdout(&wt, &["rev-parse", "HEAD"]);
    assert_ne!(
        main_sha, topic_sha,
        "fixture: the two worktrees must differ"
    );
    assert_eq!(git_cache::head_sha_from_files(&wt), Some(topic_sha));
    assert_eq!(git_cache::head_sha_from_files(tmp.path()), Some(main_sha));
}

#[test]
fn test_head_sha_from_files_detached_linked_worktree_reads_worktree_head() {
    let tmp = init_repo_with_commit();
    let first = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(tmp.path().join("b.txt"), "y").unwrap();
    commit_all(tmp.path(), "second");
    let wt = tmp.path().join("wt");
    run_git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wt.to_str().unwrap(),
            &first,
        ],
    );
    assert_eq!(git_cache::head_sha_from_files(&wt), Some(first));
}

#[test]
fn test_head_sha_from_files_unborn_branch_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    run_git(tmp.path(), &["init", "-q", "-b", "main"]);
    assert_eq!(git_cache::head_sha_from_files(tmp.path()), None);
    // The public entry point keeps its pre-existing synthetic-sha fallback.
    let synthetic = git_cache::head_sha(tmp.path()).expect("synthetic sha");
    assert_eq!(synthetic.len(), 40);
}

#[test]
fn test_head_sha_from_files_outside_git_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("plain");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(git_cache::head_sha_from_files(&sub), None);
}

#[test]
fn test_common_dir_from_files_root_and_subdirectory_match_git() {
    let tmp = init_repo_with_commit();
    let sub = tmp.path().join("src/deep");
    std::fs::create_dir_all(&sub).unwrap();
    let expected = git_common_dir(tmp.path());
    assert_eq!(
        git_cache::common_dir_from_files(tmp.path()),
        Some(expected.clone())
    );
    assert_eq!(git_cache::common_dir_from_files(&sub), Some(expected));
}

#[test]
fn test_common_dir_from_files_linked_worktree_matches_git() {
    let tmp = init_repo_with_commit();
    let wt = tmp.path().join("wt");
    run_git(
        tmp.path(),
        &["worktree", "add", "-q", "-b", "topic", wt.to_str().unwrap()],
    );
    let expected = git_common_dir(&wt);
    assert_eq!(expected, git_common_dir(tmp.path()));
    assert_eq!(git_cache::common_dir_from_files(&wt), Some(expected));
}

#[test]
fn test_common_dir_from_files_outside_git_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(git_cache::common_dir_from_files(tmp.path()), None);
}

#[test]
fn test_public_entry_points_agree_with_git() {
    let tmp = init_repo_with_commit();
    let sha = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    assert_eq!(git_cache::head_sha(tmp.path()), Some(sha));
    let common = std::fs::canonicalize(git_cache::common_dir(tmp.path()).unwrap()).unwrap();
    assert_eq!(common, git_common_dir(tmp.path()));
}
