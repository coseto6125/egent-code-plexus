//! Integration tests for `ecp tool-map`.
//!
//! Fixture: a small TS file calling `axios.get`, `fetch`, plus a
//! non-client helper so we can assert the catalog filter is exact and
//! not just a substring match.

mod common;

use common::run_git;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
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

fn setup_repo(repo: &Path, home: &Path, src: &str, origin: &str) {
    setup_repo_with_file(repo, home, "src/main.ts", src, origin);
}

/// Generalised fixture: writes `src` to `rel_path` (creating parent dirs),
/// initialises a git repo, and indexes it under the given `home` so
/// `ecp tool-map` can resolve the registry entry.
fn setup_repo_with_file(repo: &Path, home: &Path, rel_path: &str, src: &str, origin: &str) {
    let target = repo.join(rel_path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&target, src).unwrap();
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
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("admin index spawn failed");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_tool_map(repo: &Path, home: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["tool-map", "--repo", ".", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(ecp_bin())
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

// New coverage for the package-import rewrite: catches calls the
// callee-name catalog used to miss.
const FIXTURE_ALIAS_AND_ANY_METHOD: &str = r#"
import req from "axios";
import { get as gg } from "got";

export async function listSomething() {
    const a = await req.head("/probe");           // alias + method not in old catalog
    const b = await req.options("/opts");          // alias + method not in old catalog
    const c = await gg("/items");                  // named import alias bare call
    return [a, b, c];
}
"#;

#[test]
fn tool_map_tracks_alias_and_any_method() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(
        repo.path(),
        home.path(),
        FIXTURE_ALIAS_AND_ANY_METHOD,
        "git@github.com:E-NoR/tool-map-alias-test.git",
    );
    let json = run_tool_map(repo.path(), home.path(), &[]);
    let callees = callee_names(&json["calls"]["http"]);
    assert!(
        callees.contains(&"req.head"),
        "expected `req.head` (aliased default import + method): {callees:?}"
    );
    assert!(
        callees.contains(&"req.options"),
        "expected `req.options` (aliased default + method not in old catalog): {callees:?}"
    );
    assert!(
        callees.contains(&"gg"),
        "expected `gg` (named-import alias, bare call): {callees:?}"
    );
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

// ─── Cross-language / cross-category coverage ───────────────────────────────
//
// The 4 tests above all use TypeScript fixtures (only the HTTP bucket). The
// package-import scanner supports four languages × four categories — these
// per-axis tests cover the remaining permutations so a future grammar /
// parser change or PACKAGE_CATEGORY edit can't silently regress one.

/// Set up a one-file fixture, run `ecp tool-map`, assert that `callee`
/// appears under `category`. Six tests below differ only in (file path,
/// fixture, origin, category, expected callee) — extracting this helper
/// keeps each test to a three-line declaration.
fn assert_callee_present(rel_path: &str, src: &str, origin: &str, category: &str, callee: &str) {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo_with_file(repo.path(), home.path(), rel_path, src, origin);

    let json = run_tool_map(repo.path(), home.path(), &[]);
    let callees = callee_names(&json["calls"][category]);
    assert!(
        callees.contains(&callee),
        "expected `{callee}` in {category} category for {rel_path}: {callees:?}"
    );
}

#[test]
fn tool_map_python_requests_detected() {
    assert_callee_present(
        "src/main.py",
        "import requests\n\ndef fetch_user(uid):\n    r = requests.get(f\"/api/users/{uid}\")\n    return r\n",
        "git@github.com:E-NoR/tool-map-py-test.git",
        "http",
        "requests.get",
    );
}

#[test]
fn tool_map_go_net_http_detected() {
    assert_callee_present(
        "main.go",
        "package main\n\nimport (\n    \"net/http\"\n)\n\nfunc fetchOrder() {\n    http.Get(\"/api/orders\")\n}\n",
        "git@github.com:E-NoR/tool-map-go-test.git",
        "http",
        "http.Get",
    );
}

// `use reqwest::get;` then bare `get("...")` is the Rust idiom the binding
// scanner can detect — `use reqwest::Client; Client::new()` won't match
// because the scanner looks for `binding.method` / `binding(...)`, not the
// `binding::method` path syntax. Documented limitation, not a regression.
#[test]
fn tool_map_rust_reqwest_detected() {
    assert_callee_present(
        "src/main.rs",
        "use reqwest::get;\n\npub async fn fetch_data() {\n    let _ = get(\"https://example.com/api\").await;\n}\n",
        "git@github.com:E-NoR/tool-map-rs-test.git",
        "http",
        "get",
    );
}

#[test]
fn tool_map_db_category_detected_via_pg() {
    assert_callee_present(
        "src/main.ts",
        "import pg from \"pg\";\n\nexport async function dbQuery() {\n    const r = await pg.connect();\n    return r;\n}\n",
        "git@github.com:E-NoR/tool-map-db-test.git",
        "db",
        "pg.connect",
    );
}

#[test]
fn tool_map_redis_category_detected() {
    assert_callee_present(
        "src/main.ts",
        "import redis from \"redis\";\n\nexport async function cacheSet(k: string, v: string) {\n    await redis.set(k, v);\n}\n",
        "git@github.com:E-NoR/tool-map-redis-test.git",
        "redis",
        "redis.set",
    );
}

#[test]
fn tool_map_queue_category_detected_via_celery() {
    assert_callee_present(
        "src/main.py",
        "from celery import Celery\n\napp = Celery(\"my_app\")\n",
        "git@github.com:E-NoR/tool-map-queue-test.git",
        "queue",
        "Celery",
    );
}
