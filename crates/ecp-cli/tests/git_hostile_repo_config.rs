//! A scanned repository is untrusted input. Its `.git/config` and
//! `.gitattributes` can name programs for git to run, and a user-supplied
//! revision can carry an option that makes git write a file.
//!
//! Each test builds the hostile repo, runs the real code path, and asserts on
//! the side effect the attack is after — the marker file, the victim file —
//! not on the flag list. Asserting the flag list would pass for any spelling
//! of the flag, including one git ignores.

use ecp_cli::git::{DiffScope, GitDiffProvider, ShellGitProvider};
use std::fs;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo with two commits, so `diff HEAD~1 HEAD` has content to render.
fn repo_with_two_commits(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "."]);
    git(dir, &["config", "user.email", "t@example.invalid"]);
    git(dir, &["config", "user.name", "t"]);
    fs::write(dir.join("f.txt"), "a\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "one"]);
    fs::write(dir.join("f.txt"), "b\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "two"]);
}

/// A script that records the fact it ran, then behaves like the tool git
/// expected. `marker` is absolute so the payload writes it wherever git runs.
fn payload_script(path: &Path, marker: &Path) {
    fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf 'executed' > '{}'\ncat \"$1\" 2>/dev/null\nexit 0\n",
            marker.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn diff_external_from_repo_config_does_not_execute() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let marker = tmp.path().join("external-ran");
    let script = tmp.path().join("payload.sh");
    payload_script(&script, &marker);
    git(
        &repo,
        &["config", "diff.external", script.to_str().unwrap()],
    );

    let _ = ShellGitProvider.diff(&repo, &DiffScope::Compare("HEAD~1".into()));

    assert!(
        !marker.exists(),
        "diff.external from the scanned repo's own config executed: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
}

#[cfg(unix)]
#[test]
fn textconv_from_repo_config_does_not_execute() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let marker = tmp.path().join("textconv-ran");
    let script = tmp.path().join("payload.sh");
    payload_script(&script, &marker);
    fs::write(repo.join(".gitattributes"), "f.txt diff=evil\n").unwrap();
    git(
        &repo,
        &["config", "diff.evil.textconv", script.to_str().unwrap()],
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "attrs"]);
    fs::write(repo.join("f.txt"), "c\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "three"]);

    let _ = ShellGitProvider.diff(&repo, &DiffScope::Compare("HEAD~1".into()));

    assert!(
        !marker.exists(),
        "a textconv driver named by the scanned repo executed: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
}

#[test]
fn option_shaped_baseline_is_rejected_before_git_sees_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let victim = tmp.path().join("victim.txt");
    fs::write(&victim, "original content").unwrap();

    let baseline = format!("--output={}", victim.display());
    let result = ShellGitProvider.diff(&repo, &DiffScope::Compare(baseline));

    assert!(
        result.is_err(),
        "an option-shaped revision must be rejected, not passed to git"
    );
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        "original content",
        "git wrote its output over the victim file"
    );
}

#[test]
fn ordinary_revisions_still_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let diffs = ShellGitProvider
        .diff(&repo, &DiffScope::Compare("HEAD~1".into()))
        .expect("a plain revision must still work");
    assert_eq!(
        diffs
            .iter()
            .map(|d| d.file_path.as_str())
            .collect::<Vec<_>>(),
        vec!["f.txt"],
        "the hardening must not change what a normal diff reports"
    );
}
