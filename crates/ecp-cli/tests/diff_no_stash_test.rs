//! `ecp diff` builds its baseline from a scratch checkout. The user's dirty
//! file, staged change, untracked file and stash list must be byte-identical
//! afterwards, on success and on failure. Contract: nothing the user typed is
//! reshaped by a read-only query.
//!
//! The old implementation ran `git stash -u` / `checkout` / bare `stash pop`,
//! which turns a staged edit into an unstaged one.

use std::path::Path;
use std::process::{Command, Output};

mod common;
use common::{commit_all, ecp_bin, run_git};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn snapshot(repo: &Path) -> Vec<String> {
    vec![
        // `.ecp/` is ecp's own state dir, not the user's work.
        git(
            repo,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "-uall",
                "--",
                ".",
                ":(exclude).ecp",
            ],
        ),
        git(repo, &["diff"]),
        git(repo, &["diff", "--cached"]),
        git(repo, &["stash", "list", "--format=%H %gs"]),
        git(repo, &["rev-parse", "HEAD"]),
        std::fs::read_to_string(repo.join("src/f.ts")).unwrap(),
        std::fs::read_to_string(repo.join("notes.txt")).unwrap(),
    ]
}

/// Two commits, one pre-existing user stash, a staged edit with a further
/// unstaged edit on the same file, and an untracked file. Returns the SHA of
/// the first commit.
fn dirty_repo(repo: &Path) -> String {
    run_git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/f.ts"), "function foo() { return 1; }\n").unwrap();
    commit_all(repo, "v1");
    let baseline = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    std::fs::write(
        repo.join("src/f.ts"),
        "function foo() { return 2; }\nfunction baz() {}\n",
    )
    .unwrap();
    commit_all(repo, "v2");

    std::fs::write(repo.join("src/f.ts"), "// stashed\n").unwrap();
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "stash",
            "push",
            "-qm",
            "users-own-stash",
        ],
    );
    std::fs::write(repo.join("src/f.ts"), "function foo() { return 3; }\n").unwrap();
    run_git(repo, &["add", "src/f.ts"]);
    std::fs::write(repo.join("src/f.ts"), "function foo() { return 4; }\n").unwrap();
    std::fs::write(repo.join("notes.txt"), "mine\n").unwrap();
    baseline
}

fn diff_command(repo: &Path, home: &Path, baseline: &str) -> Command {
    let mut cmd = Command::new(ecp_bin());
    cmd.args([
        "diff",
        "--section",
        "symbols",
        "--baseline",
        baseline,
        "--format",
        "json",
    ])
    .current_dir(repo)
    .env("HOME", home)
    .env("ECP_NO_PROGRESS", "1");
    cmd
}

fn ecp_diff(repo: &Path, home: &Path, baseline: &str) -> Output {
    diff_command(repo, home, baseline)
        .output()
        .expect("ecp diff failed to spawn")
}

#[test]
fn test_diff_dirty_tree_success_leaves_tree_index_and_stash_byte_identical() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let baseline = dirty_repo(repo.path());
    let before = snapshot(repo.path());

    let out = ecp_diff(repo.path(), home.path(), &baseline);

    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(snapshot(repo.path()), before);
    let worktrees = git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert_eq!(
        worktrees.matches("worktree ").count(),
        1,
        "baseline worktree leaked: {worktrees}"
    );
}

#[test]
fn test_diff_unknown_baseline_fails_and_leaves_tree_index_and_stash_byte_identical() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    dirty_repo(repo.path());
    let before = snapshot(repo.path());

    let out = ecp_diff(repo.path(), home.path(), "no-such-ref-xyz");

    assert!(!out.status.success());
    assert_eq!(snapshot(repo.path()), before);
}

#[test]
fn test_diff_two_concurrent_runs_both_succeed_and_leave_tree_untouched() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let baseline = dirty_repo(repo.path());
    let before = snapshot(repo.path());

    let mut a = diff_command(repo.path(), home.path(), &baseline)
        .spawn()
        .unwrap();
    let mut b = diff_command(repo.path(), home.path(), &baseline)
        .spawn()
        .unwrap();
    let (sa, sb) = (a.wait().unwrap(), b.wait().unwrap());

    assert!(sa.success() && sb.success());
    assert_eq!(snapshot(repo.path()), before);
}
