//! `GIT_DIR` and friends redirect git away from the on-disk layout, so the
//! file readers decline and `head_file_mtime` falls back to asking git. That
//! fallback must ask for BOTH paths: `--git-dir` is the worktree's own gitdir,
//! where `HEAD` lives, while `--git-common-dir` answers with the main
//! worktree's `.git` even under a linked worktree's own `GIT_DIR`. Collapsing
//! the two puts the sentinel back on the main worktree's branch, where a
//! checkout in the linked worktree never invalidates the cached sha.
//!
//! Own test binary: the fixture sets a process-wide environment variable, and
//! every other test that reaches `git_env_overrides_present` would see it.

use ecp_cli::git_cache;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        // The fixture's own mutations must not run under the override the
        // test installs; only the code under test should see it.
        .env_remove("GIT_DIR")
        .current_dir(repo)
        .output()
        .expect("git spawn");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(repo: &Path, file: &str, msg: &str) -> String {
    std::fs::write(repo.join(file), msg).unwrap();
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            msg,
        ],
    );
    git(repo, &["rev-parse", "HEAD"])
}

#[test]
fn head_sha_under_git_dir_override_still_follows_the_linked_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let first = commit(&repo, "a.txt", "one");
    let second = commit(&repo, "b.txt", "two");

    let wt = tmp.path().join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wt.to_str().unwrap(),
            &first,
        ],
    );
    let wt_gitdir = git(&wt, &["rev-parse", "--git-dir"]);
    let common = git(&wt, &["rev-parse", "--git-common-dir"]);
    assert_ne!(
        wt_gitdir, common,
        "fixture: a linked worktree must have its own gitdir"
    );

    // From here the code under test sees the override a git hook would set.
    std::env::set_var("GIT_DIR", &wt_gitdir);

    assert_eq!(git_cache::head_sha(&wt), Some(first.clone()));
    assert_eq!(
        git_cache::git_dir(&wt).unwrap().to_string_lossy(),
        wt_gitdir,
        "the fallback must resolve the worktree's own gitdir, not the shared one"
    );

    // mtime is the sentinel and a filesystem may hold it at one-second
    // granularity, so a change inside the same second reads as no change.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    git(&wt, &["checkout", "-q", "--detach", &second]);

    assert_eq!(
        git_cache::head_sha(&wt),
        Some(second),
        "a checkout in the linked worktree must invalidate the cached HEAD \
         even when GIT_DIR sends the readers to the spawn"
    );
    std::env::remove_var("GIT_DIR");
}
