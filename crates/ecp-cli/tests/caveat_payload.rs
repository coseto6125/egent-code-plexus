//! F6: a caveat (warm-attached graph, unreadable dirty-set paths) travels in
//! the structured payload's `result` field for the commands that printed none
//! before: `routes`, `rename`, `pattern`, `processes`, `review`.
//!
//! Every test plants one fact and reads the field an LLM reads. Before F6 these
//! commands called `emit` with no caveat source, so the payload had no `result`
//! at all; the skipped-path test also failed because `EnsureResult::Ready`
//! dropped the dirty set's skipped entries.

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::path::Path;
use std::process::{Command, Output};

const WARM_ATTACH_NOTE: &str = "warm-attach from a sibling commit";

fn init_repo(repo: &Path) {
    std::fs::write(repo.join("lib.rs"), "pub fn alpha() {}\n").unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "init");
}

fn run_ecp(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(ecp_bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .expect("ecp failed to spawn")
}

fn index_repo(repo: &Path, home: &Path) {
    let out = run_ecp(repo, home, &["admin", "index", "--repo", "."]);
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// HEAD one commit past the indexed SHA, inside the warm-attach distance gate,
/// without a rebuild: the next query warm-attaches. `beta` exists only in the
/// new commit, so the sibling graph cannot know it.
fn make_warm_attached(repo: &Path) {
    std::fs::write(repo.join("extra.rs"), "pub fn beta() {}\n").unwrap();
    commit_all(repo, "advance");
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    index_repo(repo.path(), home.path());
    (repo, home)
}

fn json_payload(out: &Output, what: &str) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{what} stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn assert_warm_attach_caveat(payload: &serde_json::Value, what: &str) {
    let result = payload["result"].as_str().unwrap_or_default();
    assert!(
        result.contains(WARM_ATTACH_NOTE),
        "{what}: payload `result` must name the warm-attach, got: {payload}"
    );
}

/// The reviewer repro: a symbol the new commit added is absent from the
/// sibling graph, so the rename reports no occurrences. The payload must say
/// the graph is a warm-attach. Fails today: the "No occurrences" branch prints
/// plain text and no payload.
#[test]
fn test_rename_dry_run_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["rename", "beta", "beta2", "--dry-run"],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("No occurrences"), "setup: {stdout}");
    assert!(
        stdout.contains("result") && stdout.contains(WARM_ATTACH_NOTE),
        "the toon payload must carry `result`, got: {stdout}"
    );
}

/// A rename that does find the symbol still ends in the verification payload,
/// which must carry the caveat too. Fails today: `emit_verification_payload`
/// has no caveat source.
#[test]
fn test_rename_dry_run_found_symbol_warm_attached_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["rename", "alpha", "alpha2", "--dry-run"],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("rename_count"), "setup: {stdout}");
    assert!(stdout.contains(WARM_ATTACH_NOTE), "got: {stdout}");
}

#[test]
fn test_routes_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(repo.path(), home.path(), &["routes", "--format", "json"]);
    assert_warm_attach_caveat(&json_payload(&out, "routes"), "routes");
}

#[test]
fn test_pattern_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &[
            "pattern",
            "-p",
            "fn $N() {}",
            "--lang",
            "rs",
            "--format",
            "json",
        ],
    );
    assert_warm_attach_caveat(&json_payload(&out, "pattern"), "pattern");
}

#[test]
fn test_processes_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(repo.path(), home.path(), &["processes", "--format", "json"]);
    assert_warm_attach_caveat(&json_payload(&out, "processes"), "processes");
}

#[test]
fn test_review_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["review", "--files", "lib.rs", "--format", "json"],
    );
    assert_warm_attach_caveat(&json_payload(&out, "review"), "review");
}

/// Control: a current graph adds no field, so reliable answers pay no tokens.
#[test]
fn test_routes_current_graph_payload_has_no_result_field() {
    let (repo, home) = fixture();
    let out = run_ecp(repo.path(), home.path(), &["routes", "--format", "json"]);
    let payload = json_payload(&out, "routes");
    assert!(payload.get("result").is_none(), "got: {payload}");
}

/// One fact: an untracked file whose name is not UTF-8, on a graph built at
/// HEAD. Git prints it, the dirty set cannot name it, and no event remains, so
/// the gate answers `Ready`. Fails today: `Ready` dropped the skipped entry and
/// `ReadContext::caveats()` saw an empty set.
#[cfg(unix)]
#[test]
fn test_routes_ready_graph_with_non_utf8_dirty_path_payload_has_caveat() {
    use std::os::unix::ffi::OsStrExt;
    let (repo, home) = fixture();
    let name = std::ffi::OsStr::from_bytes(b"caf\xe9.rs");
    std::fs::write(repo.path().join(name), "pub fn gamma() {}\n").unwrap();
    let out = run_ecp(repo.path(), home.path(), &["routes", "--format", "json"]);
    let payload = json_payload(&out, "routes");
    let result = payload["result"].as_str().unwrap_or_default();
    assert!(
        result.contains("not UTF-8"),
        "the skipped path must reach the payload, got: {payload}"
    );
}
