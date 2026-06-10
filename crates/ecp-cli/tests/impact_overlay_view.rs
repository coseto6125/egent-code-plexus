//! FU-2026-06-10-398a846bca42 root-cure phase 2: `impact` must traverse the
//! query-time OverlayView so uncommitted working-tree edits are visible:
//!
//! - a NEW caller written into a dirty file shows up upstream,
//! - a RENAMED caller never leaves a phantom (old name) in the caller set,
//! - a DELETED target reports "not found" instead of answering from the
//!   stale base graph,
//! - a NEW symbol can itself be the impact target.
//!
//! All scenarios use a fixed ECP_SESSION_ID so the overlay writer (inside
//! `ensure_fresh`) and the query reader resolve the same session dir. Edits
//! are made to TRACKED files (modified-but-uncommitted) — the untracked-file
//! porcelain fix ships separately in PR #554.

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::fs;
use std::path::Path;
use std::process::Command;

const SESSION: &str = "impact-overlay-test-session";

/// Two committed files: `lib.rs` holds the target + one same-file caller;
/// `other.rs` holds a clean cross-file caller (pins the masking invariant:
/// base IN-edges from clean files must survive a dirty-file edit).
fn init_and_index(repo: &Path, home: &Path) {
    fs::write(
        repo.join("lib.rs"),
        "pub fn target_fn() {}\npub fn caller_one() { target_fn(); }\n",
    )
    .unwrap();
    fs::write(
        repo.join("other.rs"),
        "pub fn clean_caller() { target_fn(); }\n",
    )
    .unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "init");
    let out = ecp(repo, home, &["admin", "index", "--repo", "."]);
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ecp(repo: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SESSION_ID", SESSION)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .expect("ecp failed to spawn")
}

fn impact_json(repo: &Path, home: &Path, target: &str, direction: &str) -> serde_json::Value {
    let out = ecp(
        repo,
        home,
        &[
            "impact",
            "--target",
            target,
            "--direction",
            direction,
            "--format",
            "json",
        ],
    );
    assert!(
        out.status.success(),
        "impact {target} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "non-JSON impact output ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn impact_names(v: &serde_json::Value) -> Vec<String> {
    v["impact"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
        .filter_map(|e| e["name"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn new_caller_in_dirty_file_is_visible_upstream() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    // Append a brand-new caller to the TRACKED lib.rs (uncommitted edit).
    fs::write(
        repo_tmp.path().join("lib.rs"),
        "pub fn target_fn() {}\npub fn caller_one() { target_fn(); }\n\
         pub fn brand_new_caller() { target_fn(); }\n",
    )
    .unwrap();

    let v = impact_json(repo_tmp.path(), home_tmp.path(), "target_fn", "upstream");
    let names = impact_names(&v);
    assert!(
        names.iter().any(|n| n == "brand_new_caller"),
        "a caller added in an uncommitted edit must appear upstream: {names:?}"
    );
    // Clean cross-file caller still reachable through base IN-edges of the
    // replaced target node.
    assert!(
        names.iter().any(|n| n == "clean_caller"),
        "clean-file base callers must survive the overlay merge: {names:?}"
    );
    // Same-file pre-existing caller arrives via the overlay (its base edge is
    // masked, the re-parsed fragment re-emits it).
    assert!(
        names.iter().any(|n| n == "caller_one"),
        "pre-existing same-file caller must be re-resolved from the fragment: {names:?}"
    );
}

#[test]
fn renamed_caller_leaves_no_phantom() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    // Rename caller_one → renamed_caller in the working tree.
    fs::write(
        repo_tmp.path().join("lib.rs"),
        "pub fn target_fn() {}\npub fn renamed_caller() { target_fn(); }\n",
    )
    .unwrap();

    let v = impact_json(repo_tmp.path(), home_tmp.path(), "target_fn", "upstream");
    let names = impact_names(&v);
    assert!(
        names.iter().any(|n| n == "renamed_caller"),
        "the renamed caller must appear upstream: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "caller_one"),
        "the OLD name must not appear — phantom caller from the stale base: {names:?}"
    );
}

/// Pins the masking invariant directly: caller_one still EXISTS in the dirty
/// file but no longer calls target_fn. An implementation that reads base
/// IN-edges without masking dirty-file sources would report it as a phantom
/// caller; the clean-file caller must survive at the same time.
#[test]
fn dropped_call_in_dirty_file_leaves_no_phantom() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    // caller_one survives the edit but its call to target_fn is gone.
    fs::write(
        repo_tmp.path().join("lib.rs"),
        "pub fn target_fn() {}\npub fn caller_one() {}\n",
    )
    .unwrap();

    let v = impact_json(repo_tmp.path(), home_tmp.path(), "target_fn", "upstream");
    let names = impact_names(&v);
    assert!(
        !names.iter().any(|n| n == "caller_one"),
        "a dropped call must not survive via the stale base edge: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "clean_caller"),
        "the clean-file caller must remain visible: {names:?}"
    );
}

#[test]
fn deleted_target_is_reported_missing() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    // Delete target_fn from the working tree (callers now dangle; the file
    // still parses).
    fs::write(repo_tmp.path().join("lib.rs"), "pub fn caller_one() {}\n").unwrap();

    let out = ecp(
        repo_tmp.path(),
        home_tmp.path(),
        &[
            "impact",
            "--target",
            "target_fn",
            "--direction",
            "upstream",
            "--format",
            "json",
        ],
    );
    // The base graph still carries target_fn, but on disk it is gone — a
    // truthful impact answer is "no such symbol", not a stale caller list.
    // (clean_caller still calls it from a clean file, but the definition
    // no longer exists.)
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() || stdout.contains("No symbol") || stderr.contains("No symbol"),
        "a deleted symbol must not answer from the stale base graph: \
         status={} stdout={stdout} stderr={stderr}",
        out.status
    );
}

/// The canonical agent workflow: Write a brand-new (untracked) file, then ask
/// for blast radius. Exercises PR #554's porcelain `-uall` dirty set feeding
/// this PR's merged traversal end to end.
#[test]
fn caller_in_untracked_file_is_visible_upstream() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::write(
        repo_tmp.path().join("newfile.rs"),
        "pub fn untracked_caller() { target_fn(); }\n",
    )
    .unwrap();

    let v = impact_json(repo_tmp.path(), home_tmp.path(), "target_fn", "upstream");
    let names = impact_names(&v);
    assert!(
        names.iter().any(|n| n == "untracked_caller"),
        "a caller in a brand-new untracked file must appear upstream: {names:?}"
    );
}

#[test]
fn new_symbol_is_a_valid_downstream_target() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::write(
        repo_tmp.path().join("lib.rs"),
        "pub fn target_fn() {}\npub fn caller_one() { target_fn(); }\n\
         pub fn brand_new_caller() { target_fn(); }\n",
    )
    .unwrap();

    let v = impact_json(
        repo_tmp.path(),
        home_tmp.path(),
        "brand_new_caller",
        "downstream",
    );
    let names = impact_names(&v);
    assert!(
        names.iter().any(|n| n == "target_fn"),
        "downstream from a symbol that only exists in the working tree \
         must reach its callees: {names:?}"
    );
}
