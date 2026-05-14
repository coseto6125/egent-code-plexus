//! Integration tests for `gnx tool_map`.
//!
//! Fixture: a small TS file calling `axios.get`, `fetch`, plus a
//! non-client helper so we can assert the catalog filter is exact and
//! not just a substring match.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

// `axios.get` / `fetch` are both in the HTTP catalog. `compute()` is a
// local function — it must NOT show up in totals so we can confirm the
// catalog is doing real filtering (not "every CALLS edge").
const FIXTURE_WITH_CLIENTS: &str = r#"
import axios from "axios";

export async function fetchUser(id: string) {
    const r = await axios.get(`/api/users/${id}`);
    return r;
}

export async function fetchOrder(id: string) {
    const r = await fetch(`/api/orders/${id}`);
    return r;
}

export function caller() {
    return compute();
}

function compute(): number {
    return 1;
}
"#;

// No client calls at all — only local helpers calling each other.
const FIXTURE_NO_CLIENTS: &str = r#"
export function caller() {
    return compute();
}

function compute(): number {
    return 1;
}
"#;

fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git spawn failed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn setup_repo(repo: &Path, home: &Path, src: &str, origin: &str) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/main.ts"), src).unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(repo, &["remote", "add", "origin", origin]);
    run_git(repo, &["add", "-A"]);
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let out = Command::new(gnx_bin())
        .args(["analyze", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("analyze spawn failed");
    assert!(
        out.status.success(),
        "analyze failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_tool_map(repo: &Path, home: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["tool_map", "--repo", ".", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(gnx_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("tool_map spawn failed");
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} returned non-JSON: {stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("{args:?} JSON parse failed: {e}\nstdout={stdout}"))
}

fn callee_names(category: &Value) -> Vec<&str> {
    category
        .as_array()
        .map(|arr| arr.iter().filter_map(|e| e["callee"].as_str()).collect())
        .unwrap_or_default()
}

#[test]
fn tool_map_groups_http_calls_by_category() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(
        repo.path(),
        home.path(),
        FIXTURE_WITH_CLIENTS,
        "git@github.com:E-NoR/tool-map-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &[]);
    assert_eq!(json["status"], "success", "expected status=success: {json}");

    let http_count = json["totals"]["http"]
        .as_u64()
        .unwrap_or_else(|| panic!("missing totals.http: {json}"));
    assert!(
        http_count >= 2,
        "expected ≥2 http matches (axios.get + fetch), got {http_count}: {json}"
    );

    let http_callees = callee_names(&json["calls"]["http"]);
    assert!(
        http_callees.contains(&"axios.get"),
        "expected axios.get in http callees, got: {http_callees:?}"
    );
    assert!(
        http_callees.contains(&"fetch"),
        "expected fetch in http callees, got: {http_callees:?}"
    );
}

#[test]
fn tool_map_category_filter_drops_others() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(
        repo.path(),
        home.path(),
        FIXTURE_WITH_CLIENTS,
        "git@github.com:E-NoR/tool-map-filter-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &["--category", "http"]);
    assert_eq!(json["status"], "success");

    // http bucket should be populated.
    let http_count = json["totals"]["http"].as_u64().unwrap_or(0);
    assert!(
        http_count >= 2,
        "expected ≥2 http matches under --category http, got {http_count}: {json}"
    );

    // Other categories must be absent (filtered out entirely) — we
    // pre-seed only allowed buckets, so the keys themselves should be
    // missing from the totals map.
    for cat in ["db", "redis", "queue"] {
        assert!(
            json["totals"].get(cat).is_none(),
            "category {cat} leaked into --category http output: {json}"
        );
        assert!(
            json["calls"].get(cat).is_none(),
            "calls.{cat} leaked into --category http output: {json}"
        );
    }
}

#[test]
fn tool_map_empty_when_no_clients_used() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(
        repo.path(),
        home.path(),
        FIXTURE_NO_CLIENTS,
        "git@github.com:E-NoR/tool-map-empty-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &[]);
    assert_eq!(json["status"], "success");

    for cat in ["http", "db", "redis", "queue"] {
        assert_eq!(
            json["totals"][cat].as_u64().unwrap_or(0),
            0,
            "category {cat} should be 0 when no clients are used: {json}"
        );
    }
}
