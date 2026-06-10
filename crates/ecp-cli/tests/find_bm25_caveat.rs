//! FU-2026-05-29-010: the bm25 `find` paths must carry the warm-attach
//! staleness caveat like every other query verb. Single-repo bm25 reuses the
//! engine's own caveat; the cross-repo path must say WHICH of the N repos is
//! the stale one — a blanket warning would poison trust in the fresh repos'
//! rows, and silence would let a stale `found: nothing` read as definitive.

mod common;

use common::{ecp_bin, run_git};
use std::path::Path;
use std::process::Command;

fn commit_all(repo: &Path, msg: &str) {
    run_git(repo, &["add", "."]);
    run_git(
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
}

fn init_repo(repo: &Path, marker_fn: &str) {
    std::fs::write(repo.join("lib.rs"), format!("pub fn {marker_fn}() {{}}\n")).unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "init");
}

fn index_repo(repo: &Path, home: &Path) {
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Advance HEAD one commit past the indexed SHA (within the warm-attach
/// distance gate) WITHOUT rebuilding, so the next query warm-attaches.
fn make_stale(repo: &Path) {
    std::fs::write(repo.join("extra.rs"), "pub fn newer_fn() {}\n").unwrap();
    commit_all(repo, "advance");
}

fn find_bm25(cwd: &Path, home: &Path, pattern: &str, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["find", pattern, "--mode", "bm25", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(ecp_bin())
        .args(&args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .expect("find failed to spawn");
    assert!(
        out.status.success(),
        "find {extra:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "non-JSON find output ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn bm25_single_repo_fresh_graph_stays_caveat_free() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_repo(repo_tmp.path(), "fresh_marker_fn");
    index_repo(repo_tmp.path(), home_tmp.path());

    let json = find_bm25(repo_tmp.path(), home_tmp.path(), "fresh_marker_fn", &[]);
    assert!(
        json.get("result").is_none(),
        "fresh graph must not pay the caveat token cost: {json}"
    );
}

#[test]
fn bm25_single_repo_stale_graph_carries_caveat() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_repo(repo_tmp.path(), "stale_marker_fn");
    index_repo(repo_tmp.path(), home_tmp.path());
    make_stale(repo_tmp.path());

    let json = find_bm25(repo_tmp.path(), home_tmp.path(), "stale_marker_fn", &[]);
    let caveat = json
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("stale bm25 result must carry a `result` caveat: {json}"));
    assert!(
        caveat.contains("warm-attach"),
        "caveat must name the warm-attach cause: {caveat}"
    );
}

#[test]
fn bm25_cross_repo_caveat_names_only_the_stale_repo() {
    let stale_tmp = tempfile::tempdir().unwrap();
    let fresh_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();

    let stale_repo = stale_tmp.path().join("stalerepo");
    let fresh_repo = fresh_tmp.path().join("freshrepo");
    std::fs::create_dir(&stale_repo).unwrap();
    std::fs::create_dir(&fresh_repo).unwrap();

    init_repo(&stale_repo, "shared_marker_fn");
    init_repo(&fresh_repo, "shared_marker_fn");
    index_repo(&stale_repo, home_tmp.path());
    index_repo(&fresh_repo, home_tmp.path());
    make_stale(&stale_repo);

    let json = find_bm25(
        &fresh_repo,
        home_tmp.path(),
        "shared_marker_fn",
        &["--repo", "@all"],
    );
    let caveat = json
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!("cross-repo result with a stale member must carry a `result` caveat: {json}")
        });
    assert!(
        caveat.contains("stalerepo"),
        "caveat must name the stale repo: {caveat}"
    );
    assert!(
        !caveat.contains("freshrepo"),
        "caveat must NOT implicate the fresh repo: {caveat}"
    );
}

#[test]
fn bm25_cross_repo_all_fresh_stays_caveat_free() {
    let a_tmp = tempfile::tempdir().unwrap();
    let b_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();

    let repo_a = a_tmp.path().join("repoalpha");
    let repo_b = b_tmp.path().join("repobeta");
    std::fs::create_dir(&repo_a).unwrap();
    std::fs::create_dir(&repo_b).unwrap();

    init_repo(&repo_a, "shared_marker_fn");
    init_repo(&repo_b, "shared_marker_fn");
    index_repo(&repo_a, home_tmp.path());
    index_repo(&repo_b, home_tmp.path());

    let json = find_bm25(
        &repo_a,
        home_tmp.path(),
        "shared_marker_fn",
        &["--repo", "@all"],
    );
    assert!(
        json.get("result").is_none(),
        "all-fresh cross-repo result must not carry a caveat: {json}"
    );
}
