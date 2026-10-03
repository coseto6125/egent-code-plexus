//! FU-2026-05-29-010: the bm25 `find` paths must carry the warm-attach
//! staleness caveat like every other query verb. Single-repo bm25 reuses the
//! engine's own caveat; the cross-repo path must say WHICH of the N repos is
//! the stale one — a blanket warning would poison trust in the fresh repos'
//! rows, and silence would let a stale `found: nothing` read as definitive.

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::path::Path;
use std::process::Command;

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

/// Regression lock: a same-SHA rebuild publishes a `.gen.<…>` commit dir and
/// `find_latest_by_mtime` picks it. Naive dir-name suffix matching against
/// HEAD would flag this perfectly fresh repo as stale — the SHA must be
/// parsed out of the dir name (`CommitDirName::parse`) before comparing.
#[test]
fn bm25_gen_dir_for_current_head_is_not_stale() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("genrepo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo, "gen_marker_fn");
    index_repo(&repo, home_tmp.path());
    // Second build for the SAME commit → publishes a `.gen.` dir that wins
    // the latest-by-mtime pick. HEAD has not moved.
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--force", "--repo", "."])
        .current_dir(&repo)
        .env("HOME", home_tmp.path())
        .env_remove("ECP_HOME")
        .output()
        .expect("force reindex failed to spawn");
    assert!(
        out.status.success(),
        "force reindex failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let json = find_bm25(&repo, home_tmp.path(), "gen_marker_fn", &["--repo", "@all"]);
    assert!(
        json.get("result").is_none(),
        "a .gen dir for the current HEAD is fresh — no caveat: {json}"
    );
}

#[test]
fn exact_mode_rejects_registry_selector() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_repo(repo_tmp.path(), "exact_marker_fn");
    index_repo(repo_tmp.path(), home_tmp.path());

    let out = Command::new(ecp_bin())
        .args(["find", "exact_marker_fn", "--repo", "@all"])
        .current_dir(repo_tmp.path())
        .env("HOME", home_tmp.path())
        .env_remove("ECP_HOME")
        .output()
        .expect("find failed to spawn");
    assert!(
        !out.status.success(),
        "exact mode with a registry selector must fail loudly, not answer from cwd"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("only supported with"),
        "error must explain the bm25-only restriction: {stderr}"
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

/// Mark every published graph under `home` as written by an older ecp: the
/// fingerprint goes into meta.json and the sidecar, as an old binary writes
/// both. The next query must fully rebuild instead of re-attaching.
fn age_fingerprints(home: &Path) {
    let stale_fp = "v0.0.1+schema1";
    for repo_dir in std::fs::read_dir(home.join(".ecp")).unwrap().flatten() {
        let Ok(commits) = std::fs::read_dir(repo_dir.path().join("commits")) else {
            continue;
        };
        for commit in commits.flatten() {
            let meta_path = commit.path().join("meta.json");
            let Ok(text) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            let mut meta: serde_json::Value = serde_json::from_str(&text).unwrap();
            meta["builder_fingerprint"] = stale_fp.into();
            std::fs::write(&meta_path, meta.to_string()).unwrap();
            let graph = commit.path().join("graph.bin");
            std::fs::write(
                ecp_cli::auto_ensure::builder_fingerprint_sidecar_path(&graph),
                format!("{stale_fp}\n"),
            )
            .unwrap();
        }
    }
}

/// The picked graph is one commit behind HEAD and from an older ecp, so the
/// query rebuilds at HEAD and answers from it. A caveat judged on the picked
/// dir would call that fresh answer stale.
#[test]
fn bm25_single_repo_rebuilt_at_head_stays_caveat_free() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("lagrepo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo, "lag_marker_fn");
    index_repo(&repo, home_tmp.path());
    make_stale(&repo);
    age_fingerprints(home_tmp.path());

    let json = find_bm25(&repo, home_tmp.path(), "newer_fn", &["--repo", "@all"]);
    assert!(
        json.to_string().contains("newer_fn"),
        "the rebuilt HEAD graph must answer: {json}"
    );
    assert!(
        json.get("result").is_none(),
        "a graph rebuilt at HEAD is fresh — no caveat: {json}"
    );
}

#[test]
fn bm25_cross_repo_rebuilt_at_head_stays_caveat_free() {
    let lag_tmp = tempfile::tempdir().unwrap();
    let other_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let lag_repo = lag_tmp.path().join("lagrepo");
    let other_repo = other_tmp.path().join("otherrepo");
    std::fs::create_dir(&lag_repo).unwrap();
    std::fs::create_dir(&other_repo).unwrap();
    init_repo(&lag_repo, "shared_marker_fn");
    init_repo(&other_repo, "shared_marker_fn");
    index_repo(&lag_repo, home_tmp.path());
    index_repo(&other_repo, home_tmp.path());
    make_stale(&lag_repo);
    age_fingerprints(home_tmp.path());

    let json = find_bm25(
        &other_repo,
        home_tmp.path(),
        "newer_fn",
        &["--repo", "@all"],
    );
    assert!(
        json.to_string().contains("newer_fn"),
        "the rebuilt HEAD graph must answer: {json}"
    );
    assert!(
        json.get("result").is_none(),
        "graphs rebuilt at HEAD are fresh — no caveat: {json}"
    );
}
