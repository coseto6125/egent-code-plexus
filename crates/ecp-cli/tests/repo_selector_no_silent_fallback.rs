//! `--repo` names which repository the answer is about. When the value names
//! nothing, the honest outcome is an error — never an answer drawn from
//! whatever graph happened to be loaded, because the caller cannot tell the
//! difference between "here are results for the repo you asked about" and
//! "here are results for some other repo".
//!
//! `--graph` already worked this way; these tests hold `--repo` to it. Each
//! asserts on what the caller receives, so a future implementation that
//! rejects the input some other way still passes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ecp_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("ecp")
}

fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("git available")
        .success();
    assert!(ok, "git {args:?} failed");
}

/// An indexed one-file repo, plus its own `HOME` so the real `~/.ecp` is never
/// touched and the registry starts empty.
fn indexed_repo(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = tmp.join("repo");
    let home = tmp.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.invalid"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("lib.rs"), "fn only_here() {}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "init"]);

    let out = run(&repo, &home, &["admin", "index", "--repo", "."]);
    assert!(
        out.status.success(),
        "indexing the fixture failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (repo, home)
}

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(ecp_bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .output()
        .expect("run ecp")
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn impact_rejects_a_repo_that_is_not_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, home) = indexed_repo(tmp.path());

    let out = run(
        &repo,
        &home,
        &[
            "impact",
            "--target",
            "only_here",
            "--direction",
            "upstream",
            "--repo",
            "no-such-directory-here",
        ],
    );
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "answered about the current directory instead of failing: {text}"
    );
    assert!(
        text.contains("--repo"),
        "the error must name the argument at fault: {text}"
    );
}

#[test]
fn inspect_rejects_a_repo_that_is_not_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, home) = indexed_repo(tmp.path());

    let out = run(
        &repo,
        &home,
        &["inspect", "--name", "only_here", "--repo", "no-such-dir"],
    );
    assert!(
        !out.status.success(),
        "answered about the current directory instead of failing: {}",
        combined(&out)
    );
}

#[test]
fn bm25_find_rejects_a_registry_name_that_does_not_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, home) = indexed_repo(tmp.path());

    let out = run(
        &repo,
        &home,
        &[
            "find",
            "only_here",
            "--mode",
            "bm25",
            "--repo",
            "no-such-registered-repo",
        ],
    );
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "searched the current directory's graph under another repo's name: {text}"
    );
    assert!(
        text.contains("registry"),
        "the error must say the name is not registered: {text}"
    );
}

#[test]
fn a_repo_path_still_answers_about_that_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, home) = indexed_repo(tmp.path());
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    // Run from a directory that is not the repo, and point at it by path.
    let out = run(
        &elsewhere,
        &home,
        &["find", "only_here", "--repo", repo.to_str().unwrap()],
    );
    let text = combined(&out);
    assert!(
        out.status.success(),
        "a --repo path must keep working: {text}"
    );
    assert!(
        text.contains("only_here"),
        "expected the symbol from the named repo: {text}"
    );
}
