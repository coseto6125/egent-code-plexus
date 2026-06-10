//! FU-2026-06-10-398a846bca42 root-cure phase 3: `cypher` must answer from
//! the merged graph (base CSR + OverlayView), mirroring impact's phase 2:
//! symbols and Calls edges from uncommitted edits are visible, deleted and
//! renamed symbols leave no phantoms.
//!
//! Fixed ECP_SESSION_ID keeps the overlay writer (ensure_fresh) and the
//! query reader on one session dir.

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::fs;
use std::path::Path;
use std::process::Command;

const SESSION: &str = "cypher-overlay-test-session";

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

/// Run a cypher query, return the rows as a JSON value.
fn cypher(repo: &Path, home: &Path, q: &str) -> serde_json::Value {
    let out = ecp(repo, home, &["cypher", q, "--format", "json"]);
    assert!(
        out.status.success(),
        "cypher failed: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "non-JSON cypher output ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// Flatten all scalar string cells of the result rows.
fn string_cells(v: &serde_json::Value) -> Vec<String> {
    let rows = v["rows"]
        .as_array()
        .or_else(|| v["results"].as_array())
        .or_else(|| v["data"].as_array());
    rows.map(|rs| {
        rs.iter()
            .flat_map(|row| match row {
                serde_json::Value::Array(cells) => cells.clone(),
                serde_json::Value::Object(m) => m.values().cloned().collect(),
                other => vec![other.clone()],
            })
            .filter_map(|c| c.as_str().map(str::to_owned))
            .collect()
    })
    .unwrap_or_default()
}

#[test]
fn match_sees_symbol_in_dirty_file() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::write(
        repo_tmp.path().join("lib.rs"),
        "pub fn target_fn() {}\npub fn caller_one() { target_fn(); }\n\
         pub fn brand_new_caller() { target_fn(); }\n",
    )
    .unwrap();

    let v = cypher(
        repo_tmp.path(),
        home_tmp.path(),
        "MATCH (f:Function {name: 'brand_new_caller'}) RETURN f.name",
    );
    let cells = string_cells(&v);
    assert!(
        cells.iter().any(|c| c == "brand_new_caller"),
        "a symbol added in an uncommitted edit must MATCH: {v}"
    );
}

#[test]
fn calls_edge_from_dirty_file_is_traversable() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::write(
        repo_tmp.path().join("lib.rs"),
        "pub fn target_fn() {}\npub fn caller_one() { target_fn(); }\n\
         pub fn brand_new_caller() { target_fn(); }\n",
    )
    .unwrap();

    let v = cypher(
        repo_tmp.path(),
        home_tmp.path(),
        "MATCH (a)-[:Calls]->(b {name: 'target_fn'}) RETURN a.name",
    );
    let cells = string_cells(&v);
    assert!(
        cells.iter().any(|c| c == "brand_new_caller"),
        "overlay Calls edge must be traversable: {v}"
    );
    assert!(
        cells.iter().any(|c| c == "clean_caller"),
        "clean-file base Calls edge must survive the merge: {v}"
    );
}

#[test]
fn renamed_symbol_leaves_no_phantom_in_match() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::write(
        repo_tmp.path().join("lib.rs"),
        "pub fn target_fn() {}\npub fn renamed_caller() { target_fn(); }\n",
    )
    .unwrap();

    let v = cypher(
        repo_tmp.path(),
        home_tmp.path(),
        "MATCH (a)-[:Calls]->(b {name: 'target_fn'}) RETURN a.name",
    );
    let cells = string_cells(&v);
    assert!(
        cells.iter().any(|c| c == "renamed_caller"),
        "renamed caller must appear: {v}"
    );
    assert!(
        !cells.iter().any(|c| c == "caller_one"),
        "old name must not appear — phantom from the stale base: {v}"
    );

    let m = cypher(
        repo_tmp.path(),
        home_tmp.path(),
        "MATCH (f:Function {name: 'caller_one'}) RETURN f.name",
    );
    let m_cells = string_cells(&m);
    assert!(
        m_cells.is_empty(),
        "a renamed-away symbol must not MATCH: {m}"
    );
}

#[test]
fn deleted_symbol_does_not_match() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::write(repo_tmp.path().join("lib.rs"), "pub fn caller_one() {}\n").unwrap();

    let v = cypher(
        repo_tmp.path(),
        home_tmp.path(),
        "MATCH (f:Function {name: 'target_fn'}) RETURN f.name",
    );
    let cells = string_cells(&v);
    assert!(
        cells.is_empty(),
        "a deleted symbol must not answer from the stale base: {v}"
    );
}
