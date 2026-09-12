//! `build_l2` keys the L2 slot by commit sha only. A build started from a
//! subdirectory (a `cargo test` cwd, a hook fired from `crates/x`) used to
//! publish that subtree as the commit's graph; every later query for the sha
//! then attached to it and reported `found: false` for symbols outside the
//! subtree. The build must resolve to the worktree root first.

use ecp_cli::build::orchestrator;
use ecp_core::registry::CommitBuildMeta;
use std::path::Path;
use std::process::Command;

fn git(worktree: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(worktree)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn init_two_dir_repo(worktree: &Path) {
    std::fs::create_dir_all(worktree.join("a")).unwrap();
    std::fs::create_dir_all(worktree.join("b")).unwrap();
    std::fs::write(worktree.join("a/lib.js"), "function inA() {}\n").unwrap();
    std::fs::write(worktree.join("b/other.js"), "function inB() {}\n").unwrap();
    git(worktree, &["init", "-q"]);
    git(worktree, &["add", "."]);
    git(worktree, &["commit", "-qm", "init"]);
}

#[test]
fn test_build_l2_from_subdirectory_indexes_the_whole_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    init_two_dir_repo(&worktree);
    // Each build gets its own ecp home so the second cannot fast-path onto the first.
    std::env::set_var("HOME", tmp.path().join("home-subdir"));
    let from_subdir = orchestrator::build_l2(&worktree.join("a"), None).unwrap();
    let subdir_meta = CommitBuildMeta::read(&from_subdir.commit_dir.join("meta.json")).unwrap();
    std::env::set_var("HOME", tmp.path().join("home-root"));
    let from_root = orchestrator::build_l2(&worktree, None).unwrap();
    let root_meta = CommitBuildMeta::read(&from_root.commit_dir.join("meta.json")).unwrap();

    assert_eq!(
        subdir_meta.node_count, root_meta.node_count,
        "a build from a/ must index b/ too (subdir {} vs root {})",
        subdir_meta.node_count, root_meta.node_count
    );
    let recorded = dunce::canonicalize(&subdir_meta.built_from_worktree).unwrap();
    assert_eq!(recorded, dunce::canonicalize(&worktree).unwrap());
}

#[test]
fn test_force_rebuild_from_subdirectory_indexes_the_whole_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    init_two_dir_repo(&worktree);
    let sha = String::from_utf8(
        Command::new("git")
            .current_dir(&worktree)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    std::env::set_var("HOME", tmp.path().join("home-force"));
    let forced = ecp_cli::build::force::force_rebuild_l2(&worktree.join("b"), &sha).unwrap();
    let forced_meta = CommitBuildMeta::read(&forced.commit_dir.join("meta.json")).unwrap();
    std::env::set_var("HOME", tmp.path().join("home-root2"));
    let from_root = orchestrator::build_l2(&worktree, None).unwrap();
    let root_meta = CommitBuildMeta::read(&from_root.commit_dir.join("meta.json")).unwrap();
    assert_eq!(forced_meta.node_count, root_meta.node_count);
}
