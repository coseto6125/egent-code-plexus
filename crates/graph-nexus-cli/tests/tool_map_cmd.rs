//! Integration tests for `gnx tool-map`.
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
    setup_repo_with_file(repo, home, "src/main.ts", src, origin);
}

/// Generalised fixture: writes `src` to `rel_path` (creating parent dirs),
/// initialises a git repo, and indexes it under the given `home` so
/// `gnx tool-map` can resolve the registry entry.
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
    let out = Command::new(gnx_bin())
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

// ─── Cross-language coverage (Python / Go / Rust) ───────────────────────────
//
// The 4 tests above all use TypeScript fixtures. The package-import scanner
// supports four languages — these tests exercise the remaining three so a
// future grammar / parser change in any of them doesn't silently regress.

const FIXTURE_PYTHON_REQUESTS: &str = r#"
import requests

def fetch_user(uid):
    r = requests.get(f"/api/users/{uid}")
    return r
"#;

#[test]
fn tool_map_python_requests_detected() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo_with_file(
        repo.path(),
        home.path(),
        "src/main.py",
        FIXTURE_PYTHON_REQUESTS,
        "git@github.com:E-NoR/tool-map-py-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &[]);
    let callees = callee_names(&json["calls"]["http"]);
    assert!(
        callees.contains(&"requests.get"),
        "expected `requests.get` from Python `import requests` + `requests.get(...)`: {callees:?}"
    );
}

const FIXTURE_GO_NET_HTTP: &str = r#"
package main

import (
    "net/http"
)

func fetchOrder() {
    http.Get("/api/orders")
}
"#;

#[test]
fn tool_map_go_net_http_detected() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo_with_file(
        repo.path(),
        home.path(),
        "main.go",
        FIXTURE_GO_NET_HTTP,
        "git@github.com:E-NoR/tool-map-go-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &[]);
    let callees = callee_names(&json["calls"]["http"]);
    assert!(
        callees.contains(&"http.Get"),
        "expected `http.Get` from Go `import \"net/http\"` (basename binding `http`): {callees:?}"
    );
}

// `use reqwest::get;` then bare `get("...")` is the Rust idiom the binding
// scanner can detect — `use reqwest::Client; Client::new()` won't match
// because the scanner looks for `binding.method` / `binding(...)`, not the
// `binding::method` path syntax. Documented limitation, not a regression.
const FIXTURE_RUST_REQWEST: &str = r#"
use reqwest::get;

pub async fn fetch_data() {
    let _ = get("https://example.com/api").await;
}
"#;

#[test]
fn tool_map_rust_reqwest_detected() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo_with_file(
        repo.path(),
        home.path(),
        "src/main.rs",
        FIXTURE_RUST_REQWEST,
        "git@github.com:E-NoR/tool-map-rs-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &[]);
    let callees = callee_names(&json["calls"]["http"]);
    assert!(
        callees.contains(&"get"),
        "expected `get` from Rust `use reqwest::get;` + bare `get(...)` call: {callees:?}"
    );
}

// ─── Cross-category coverage (DB / Redis / Queue) ───────────────────────────
//
// HTTP is exercised by every test above. These three target the remaining
// PACKAGE_CATEGORY buckets so a category-name typo or PACKAGE_CATEGORY
// table edit doesn't silently drop them.

const FIXTURE_TS_PG: &str = r#"
import pg from "pg";

export async function dbQuery() {
    const r = await pg.connect();
    return r;
}
"#;

#[test]
fn tool_map_db_category_detected_via_pg() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo_with_file(
        repo.path(),
        home.path(),
        "src/main.ts",
        FIXTURE_TS_PG,
        "git@github.com:E-NoR/tool-map-db-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &[]);
    let callees = callee_names(&json["calls"]["db"]);
    assert!(
        callees.contains(&"pg.connect"),
        "expected `pg.connect` in db category from `import pg from \"pg\"`: {callees:?}"
    );
}

const FIXTURE_TS_REDIS: &str = r#"
import redis from "redis";

export async function cacheSet(k: string, v: string) {
    await redis.set(k, v);
}
"#;

#[test]
fn tool_map_redis_category_detected() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo_with_file(
        repo.path(),
        home.path(),
        "src/main.ts",
        FIXTURE_TS_REDIS,
        "git@github.com:E-NoR/tool-map-redis-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &[]);
    let callees = callee_names(&json["calls"]["redis"]);
    assert!(
        callees.contains(&"redis.set"),
        "expected `redis.set` in redis category from `import redis from \"redis\"`: {callees:?}"
    );
}

const FIXTURE_PYTHON_CELERY: &str = r#"
from celery import Celery

app = Celery("my_app")
"#;

#[test]
fn tool_map_queue_category_detected_via_celery() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo_with_file(
        repo.path(),
        home.path(),
        "src/main.py",
        FIXTURE_PYTHON_CELERY,
        "git@github.com:E-NoR/tool-map-queue-test.git",
    );

    let json = run_tool_map(repo.path(), home.path(), &[]);
    let callees = callee_names(&json["calls"]["queue"]);
    assert!(
        callees.contains(&"Celery"),
        "expected `Celery` in queue category from Python `from celery import Celery; Celery(...)`: {callees:?}"
    );
}
