//! F6: a caveat (warm-attached graph, unreadable dirty-set paths) travels in
//! the structured payload's `result` field for the commands that printed none
//! before: `routes`, `rename`, `pattern`, `processes`, `review`, then
//! `shape-check`, `tool-map`, `heuristics` and the three deprecated `find-*`
//! aliases.
//!
//! Every test plants one fact and reads the field an LLM reads. Before F6 these
//! commands called `emit` with no caveat source, so the payload had no `result`
//! at all; the skipped-path test also failed because `EnsureResult::Ready`
//! dropped the dirty set's skipped entries. The unreadable-path caveat once
//! reached only the five F6 commands: `find`, `impact` and the by-repo loader
//! read the engine's caveat, which did not hold it.

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
/// the graph is a warm-attach. Failed before F6: the "No occurrences" branch
/// printed plain text and no payload.
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
/// which must carry the caveat too. Failed before F6:
/// `emit_verification_payload` had no caveat source.
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
/// the gate answers `Ready`. Failed before F6: `Ready` dropped the skipped
/// entry, so no caveat source saw it.
#[cfg(target_os = "linux")]
#[test]
fn test_routes_ready_graph_with_non_utf8_dirty_path_payload_has_caveat() {
    let (repo, home) = fixture();
    plant_non_utf8_path(repo.path());
    let out = run_ecp(repo.path(), home.path(), &["routes", "--format", "json"]);
    assert_non_utf8_caveat(&json_payload(&out, "routes"), "routes");
}

/// The six commands below called `emit` with no caveat source. Failed before
/// this change: no `result` field.
#[test]
fn test_shape_check_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["shape-check", "--format", "json"],
    );
    assert_warm_attach_caveat(&json_payload(&out, "shape-check"), "shape-check");
}

#[test]
fn test_tool_map_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(repo.path(), home.path(), &["tool-map", "--format", "json"]);
    assert_warm_attach_caveat(&json_payload(&out, "tool-map"), "tool-map");
}

#[test]
fn test_heuristics_saga_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["heuristics", "saga", "--format", "json"],
    );
    assert_warm_attach_caveat(&json_payload(&out, "heuristics saga"), "heuristics saga");
}

#[test]
fn test_find_transaction_patterns_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["find-transaction-patterns", "--format", "json"],
    );
    assert_warm_attach_caveat(
        &json_payload(&out, "find-transaction-patterns"),
        "find-transaction-patterns",
    );
}

/// The not-found answer is the one a stale graph most often explains: the
/// payload must carry the caveat although the command exits non-zero.
#[test]
fn test_find_schema_bindings_warm_attached_not_found_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["find-schema-bindings", "User.email", "--format", "json"],
    );
    assert!(!out.status.success(), "setup: no SchemaField exists");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let payload: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout}"));
    assert_eq!(payload["status"], "not_found", "setup: {payload}");
    assert_warm_attach_caveat(&payload, "find-schema-bindings");
}

#[test]
fn test_find_event_mirrors_warm_attached_graph_payload_has_caveat() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["find-event-mirrors", "--format", "json"],
    );
    assert_warm_attach_caveat(
        &json_payload(&out, "find-event-mirrors"),
        "find-event-mirrors",
    );
}

/// `--format text` of event-mirrors prints a table, not a payload. Contract:
/// the caveat is its first line. Failed before this change: the table only.
#[test]
fn test_heuristics_event_mirrors_text_warm_attached_caveat_first_line() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["heuristics", "event-mirrors", "--format", "text"],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().unwrap_or_default();
    assert!(first.contains(WARM_ATTACH_NOTE), "got: {stdout}");
}

/// `--format text` of a `results` payload printed the rows only. Contract: the
/// caveat is the first line. Failed before this change: no caveat line.
#[test]
fn test_routes_text_warm_attached_caveat_first_line() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(repo.path(), home.path(), &["routes", "--format", "text"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().unwrap_or_default();
    assert!(first.contains(WARM_ATTACH_NOTE), "got: {stdout}");
}

/// `find --mode exact --format text` printed its caveat on stderr only, so a
/// stdout-only consumer (MCP) read a stale miss as definitive. Contract: the
/// caveat is the first stdout line.
#[test]
fn test_find_exact_text_warm_attached_caveat_first_stdout_line() {
    let (repo, home) = fixture();
    make_warm_attached(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &[
            "find",
            "no_such_symbol_zz",
            "--mode",
            "exact",
            "--format",
            "text",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().unwrap_or_default();
    assert!(first.contains(WARM_ATTACH_NOTE), "got: {stdout}");
}

/// Plant an untracked file whose name is not UTF-8 on a graph built at HEAD:
/// git prints it, the dirty set cannot name it. Linux only: APFS refuses
/// such a name (`EILSEQ`), so macOS cannot hold the file.
#[cfg(target_os = "linux")]
fn plant_non_utf8_path(repo: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::OsStr::from_bytes(b"caf\xe9.rs");
    std::fs::write(repo.join(name), "pub fn gamma() {}\n").unwrap();
}

#[cfg(target_os = "linux")]
fn assert_non_utf8_caveat(payload: &serde_json::Value, what: &str) {
    let result = payload["result"].as_str().unwrap_or_default();
    assert!(
        result.contains("not UTF-8"),
        "{what}: the skipped path must reach the payload, got: {payload}"
    );
}

/// Failed before this change: `find` read `Engine::caveat`, which held only
/// the graph's staleness, so the payload had no `result`.
#[cfg(target_os = "linux")]
#[test]
fn test_find_ready_graph_with_non_utf8_dirty_path_payload_has_caveat() {
    let (repo, home) = fixture();
    plant_non_utf8_path(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["find", "alpha", "--format", "json"],
    );
    let payload = json_payload(&out, "find");
    assert_eq!(payload["found"], true, "setup: {payload}");
    assert_non_utf8_caveat(&payload, "find");
}

#[cfg(target_os = "linux")]
#[test]
fn test_impact_ready_graph_with_non_utf8_dirty_path_payload_has_caveat() {
    let (repo, home) = fixture();
    plant_non_utf8_path(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["impact", "--target", "alpha", "--format", "json"],
    );
    assert_non_utf8_caveat(&json_payload(&out, "impact"), "impact");
}

/// A registry selector loads its engine through `load_ensured`, which kept the
/// engine and dropped the read context. Failed before this change: no
/// `result`.
#[cfg(target_os = "linux")]
#[test]
fn test_find_bm25_registry_selector_non_utf8_dirty_path_payload_has_caveat() {
    let (repo, home) = fixture();
    plant_non_utf8_path(repo.path());
    let key = std::fs::read_dir(home.path().join(".ecp"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.path().join("commits").is_dir())
        .expect("admin index registered the repo")
        .file_name()
        .into_string()
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let out = run_ecp(
        outside.path(),
        home.path(),
        &[
            "find",
            "alpha",
            "--mode",
            "bm25",
            "--repo",
            key.as_str(),
            "--format",
            "json",
        ],
    );
    assert_non_utf8_caveat(
        &json_payload(&out, "find --repo <key>"),
        "find --repo <key>",
    );
}
