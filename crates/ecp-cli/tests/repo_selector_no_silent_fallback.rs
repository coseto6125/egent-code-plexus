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

/// An indexed one-file repo, plus its own cache root so the real `~/.ecp` is
/// never touched and the registry starts empty. See `run` for why HOME alone
/// does not achieve that.
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
        // `resolve_home_ecp` reads ECP_HOME first and only falls back to HOME,
        // so overriding HOME alone leaves a developer who sets ECP_HOME
        // writing fixtures into the real cache — and asserting against their
        // real registry instead of an empty one.
        .env("HOME", home)
        .env_remove("ECP_HOME")
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

/// The registry outlives the graphs it points at: `admin gc` prunes commit
/// directories under a retention policy and leaves `registry.json` alone. A
/// name in that state passes the registry check and then produces no target,
/// which the callers read as "no selector given" — the same wrong answer by a
/// different door.
#[test]
fn a_registered_repo_with_no_index_left_is_not_answered_from_the_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, home) = indexed_repo(tmp.path());

    // Find the cache directory the fixture registered, then empty its commits
    // the way retention does.
    let ecp = home.join(".ecp");
    let dir_name = std::fs::read_dir(&ecp)
        .expect("cache root")
        .filter_map(|e| e.ok())
        .find(|e| e.path().join("commits").is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .expect("the fixture must have registered one repo");
    std::fs::remove_dir_all(ecp.join(&dir_name).join("commits")).unwrap();
    std::fs::create_dir_all(ecp.join(&dir_name).join("commits")).unwrap();

    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("other.rs"), "fn only_here() {}\n").unwrap();

    let out = run(
        &elsewhere,
        &home,
        &["find", "only_here", "--mode", "bm25", "--repo", &dir_name],
    );
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "answered from the current directory for a repo with no index: {text}"
    );
    assert!(
        text.contains(&dir_name),
        "the error must name the repo it could not search: {text}"
    );
    let _ = repo;
}
