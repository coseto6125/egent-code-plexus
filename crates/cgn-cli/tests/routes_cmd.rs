//! Integration tests for `cgn routes [<path>]`.
//!
//! Reuses the same TypeScript Express fixture as `api_impact_cmd.rs`:
//! two routes (`GET /api/users`, `POST /api/users`) each with a named
//! handler and an upstream caller function.

mod common;

use common::run_git;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

const FIXTURE_SRC: &str = r#"
import express from "express";

const app = express();

function list_users(req: any, res: any) {
    return _build_user_list();
}

function create_user(req: any, res: any) {
    return _persist_user(req.body);
}

function _build_user_list() {
    return [];
}

function _persist_user(body: any) {
    return body;
}

export function caller_listing() {
    return list_users({} as any, {} as any);
}

export function caller_creating() {
    return create_user({ body: {} } as any, {} as any);
}

app.get("/api/users", list_users);
app.post("/api/users", create_user);
"#;

fn setup_repo(repo: &Path, home: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/main.ts"), FIXTURE_SRC).unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/routes-cmd-test.git",
        ],
    );
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

/// Run `cgn routes [extra...]` and return parsed JSON output.
fn run_routes_json(repo: &Path, home: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["routes", "--repo", ".", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(gnx_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("routes spawn failed");
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

/// Run `cgn routes [extra...]` and return raw stdout as a string.
fn run_routes_stdout(repo: &Path, home: &Path, extra: &[&str]) -> String {
    let mut args = vec!["routes", "--repo", "."];
    args.extend_from_slice(extra);
    let out = Command::new(gnx_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("routes spawn failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ── list mode (no path) ──────────────────────────────────────────────────────

#[test]
fn routes_no_path_lists_all() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &[]);
    assert_eq!(json["status"], "success", "unexpected status: {json}");

    let results = json["results"].as_array().expect("results array missing");
    assert!(!results.is_empty(), "expected ≥1 route in listing: {json}");

    // Every entry must have a path and method.
    for r in results {
        assert!(r.get("path").is_some(), "route missing 'path': {r}");
        assert!(r.get("method").is_some(), "route missing 'method': {r}");
    }
}

#[test]
fn routes_no_path_contains_both_routes() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &[]);
    let results = json["results"].as_array().unwrap();
    let paths: Vec<&str> = results
        .iter()
        .map(|r| r["path"].as_str().unwrap_or(""))
        .collect();
    assert!(
        paths.contains(&"/api/users"),
        "expected /api/users in listing, got: {paths:?}"
    );
}

// ── list mode + --method filter ──────────────────────────────────────────────

#[test]
fn routes_method_filter_get_only() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["--method", "GET"]);
    assert_eq!(json["status"], "success");
    let results = json["results"].as_array().unwrap();
    for r in results {
        assert_eq!(
            r["method"].as_str().unwrap_or(""),
            "GET",
            "non-GET route leaked: {r}"
        );
    }
}

#[test]
fn routes_method_filter_post_only() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["--method", "POST"]);
    assert_eq!(json["status"], "success");
    let results = json["results"].as_array().unwrap();
    assert!(
        !results.is_empty(),
        "expected ≥1 POST route in listing: {json}"
    );
    for r in results {
        assert_eq!(
            r["method"].as_str().unwrap_or(""),
            "POST",
            "non-POST route leaked: {r}"
        );
    }
}

// ── inspect mode (with path) ─────────────────────────────────────────────────

#[test]
fn routes_with_path_finds_handler_and_callers() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/api/users"]);
    assert_eq!(json["status"], "found", "expected status=found: {json}");

    let routes = json["routes"].as_array().expect("routes array missing");
    assert!(!routes.is_empty(), "expected ≥1 matched route: {json}");

    let handlers = json["handlers"].as_array().expect("handlers array missing");
    let handler_names: Vec<&str> = handlers
        .iter()
        .map(|h| h["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        handler_names.contains(&"list_users") || handler_names.contains(&"create_user"),
        "expected list_users / create_user in handlers, got: {handler_names:?}"
    );

    let callers = json["callers"].as_array().expect("callers array missing");
    assert!(!callers.is_empty(), "expected ≥1 upstream caller: {json}");
    let caller_names: Vec<&str> = callers
        .iter()
        .map(|c| c["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        caller_names
            .iter()
            .any(|n| n.starts_with("caller_") || n.starts_with('_')),
        "expected caller_* / _build_* in callers, got: {caller_names:?}"
    );
}

#[test]
fn routes_with_path_method_filter_disambiguates() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    // --method POST: only create_user handler.
    let post_json = run_routes_json(
        repo.path(),
        home.path(),
        &["/api/users", "--method", "POST"],
    );
    assert_eq!(post_json["status"], "found");
    let post_handlers: Vec<&str> = post_json["handlers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        post_handlers.contains(&"create_user"),
        "expected create_user for POST, got: {post_handlers:?}"
    );
    assert!(
        !post_handlers.contains(&"list_users"),
        "list_users (GET) leaked into POST result: {post_handlers:?}"
    );

    // Routes array must all be POST.
    for r in post_json["routes"].as_array().unwrap() {
        assert_eq!(r["method"].as_str().unwrap_or(""), "POST");
    }
}

#[test]
fn routes_with_unknown_path_returns_not_found() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/no-such-path"]);
    assert_eq!(
        json["status"], "not_found",
        "expected status=not_found for unknown route: {json}"
    );
    assert_eq!(json["route_pattern"], "/no-such-path");
    assert!(
        json.get("candidates").is_some(),
        "candidates key missing: {json}"
    );
}

// ── empty fixture: framework hint ────────────────────────────────────────────

const EMPTY_SRC: &str = r#"
// No route declarations.
function plain_helper() {
    return 42;
}
"#;

fn setup_empty_repo(repo: &Path, home: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.ts"), EMPTY_SRC).unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/routes-empty-test.git",
        ],
    );
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

#[test]
fn routes_empty_includes_framework_hint() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_empty_repo(repo.path(), home.path());

    // Capture stderr as well — the hint goes to stderr.
    let args = ["routes", "--repo", ".", "--format", "json"];
    let out = Command::new(gnx_bin())
        .args(args)
        .current_dir(repo.path())
        .env("HOME", home.path())
        .output()
        .expect("routes spawn failed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("No HTTP routes")
            || combined.contains("framework")
            || combined.contains("coverage")
            || combined.contains("0 routes"),
        "missing framework hint on empty fixture:\nstdout={stdout}\nstderr={stderr}"
    );
}

// ── toon output sanity (default format) ─────────────────────────────────────

#[test]
fn routes_no_path_toon_output_has_route_info() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let stdout = run_routes_stdout(repo.path(), home.path(), &[]);
    // toon output should contain something recognisable — path segment or "method"/"route"
    assert!(
        stdout.contains("/api") || stdout.contains("Route") || stdout.contains("method"),
        "toon output missing route info:\n{stdout}"
    );
}

// ── inline-anonymous handler + enclosing scope ──────────────────────────────
//
// Regression fixture: a TS file with a *named* handler and an *inline arrow*
// handler, both registered inside an enclosing setup function. The named
// case must report `handlerKind: "named"`; the inline case must surface a
// synthetic handler entry (instead of an empty `handlers[]` array) tagged
// `handlerKind: "inline_anonymous"`. Both routes must carry an
// `enclosingScope` pointing back at the registration function.

const INLINE_FIXTURE_SRC: &str = r#"
import express from "express";

const app = express();

function _build_user_list() {
    return [];
}

function list_users(req: any, res: any) {
    return _build_user_list();
}

// Mixed-handler registration nested inside an enclosing setup function.
export function register_routes() {
    app.get("/api/users", list_users);
    app.get("/api/health", (_req, res) => {
        res.json({ status: "ok" });
    });
}
"#;

fn setup_inline_repo(repo: &Path, home: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/main.ts"), INLINE_FIXTURE_SRC).unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/routes-inline-test.git",
        ],
    );
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

#[test]
fn routes_inline_handler_synthesized() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_inline_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/api/health"]);
    assert_eq!(json["status"], "found", "expected status=found: {json}");

    let handlers = json["handlers"].as_array().expect("handlers array missing");
    assert!(
        !handlers.is_empty(),
        "inline-arrow route must surface a synthetic handler — empty handlers[] is the bug we are fixing: {json}"
    );
    let kinds: Vec<&str> = handlers
        .iter()
        .map(|h| h["handlerKind"].as_str().unwrap_or(""))
        .collect();
    assert!(
        kinds.contains(&"inline_anonymous"),
        "expected handlerKind=inline_anonymous, got: {kinds:?}\nfull json: {json}"
    );
}

#[test]
fn routes_named_handler_marked_named() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_inline_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/api/users"]);
    assert_eq!(json["status"], "found", "expected status=found: {json}");

    let handlers = json["handlers"].as_array().expect("handlers array missing");
    let named: Vec<&str> = handlers
        .iter()
        .filter(|h| h["handlerKind"].as_str() == Some("named"))
        .map(|h| h["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        named.contains(&"list_users"),
        "expected list_users as named handler, got: {named:?}\nfull json: {json}"
    );
}

#[test]
fn routes_inline_route_has_enclosing_scope() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_inline_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/api/health"]);
    let routes = json["routes"].as_array().expect("routes array missing");
    assert!(!routes.is_empty(), "expected ≥1 matched route: {json}");

    let scope_names: Vec<&str> = routes
        .iter()
        .filter_map(|r| r["enclosingScope"]["name"].as_str())
        .collect();
    assert!(
        scope_names.contains(&"register_routes"),
        "expected enclosingScope.name=register_routes for /api/health, got: {scope_names:?}\nfull json: {json}"
    );
}

// ── Python decorator-route handler resolution ──────────────────────────────
//
// PR #76 follow-up regression: FastAPI / Flask / Sanic-style decorator routes
// (`@router.post("/x")\ndef handler(): ...`) must resolve the handler to the
// decorated function — not fall through to `inline_anonymous`. The Python
// parser previously hard-coded `RawRoute.handler = None` for every route it
// detected, so the builder never emitted a `HandlesRoute` edge and PR #76's
// inline-anonymous fallback fired incorrectly.

const PY_DECORATOR_FIXTURE_SRC: &str = r#"
from fastapi import APIRouter

router = APIRouter()

@router.post("/chat")
def chat_endpoint(req):
    return {"reply": "hi"}

@router.get("/health")
def health_check():
    return {"status": "ok"}
"#;

fn setup_py_decorator_repo(repo: &Path, home: &Path) {
    std::fs::create_dir_all(repo.join("app")).unwrap();
    std::fs::write(repo.join("app/main.py"), PY_DECORATOR_FIXTURE_SRC).unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/routes-py-decorator-test.git",
        ],
    );
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

#[test]
fn routes_python_decorator_resolves_named_handler() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_py_decorator_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/chat"]);
    assert_eq!(json["status"], "found", "expected status=found: {json}");

    let handlers = json["handlers"].as_array().expect("handlers array missing");
    assert!(
        !handlers.is_empty(),
        "decorator route must surface a handler: {json}"
    );
    let kinds_and_names: Vec<(&str, &str)> = handlers
        .iter()
        .map(|h| {
            (
                h["handlerKind"].as_str().unwrap_or(""),
                h["name"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert!(
        kinds_and_names
            .iter()
            .any(|(k, n)| *k == "named" && *n == "chat_endpoint"),
        "expected (handlerKind=named, name=chat_endpoint) for @router.post(\"/chat\") — \
         decorator route must NOT fall through to inline_anonymous. got: {kinds_and_names:?}\nfull json: {json}"
    );
    assert!(
        !kinds_and_names
            .iter()
            .any(|(k, _)| *k == "inline_anonymous"),
        "decorator-style route was misclassified as inline_anonymous: {kinds_and_names:?}\nfull json: {json}"
    );
}

#[test]
fn routes_python_decorator_get_method_resolved() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_py_decorator_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/health"]);
    let handlers = json["handlers"].as_array().expect("handlers array missing");
    let names: Vec<&str> = handlers
        .iter()
        .filter(|h| h["handlerKind"].as_str() == Some("named"))
        .map(|h| h["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        names.contains(&"health_check"),
        "expected health_check as named handler for /health, got: {names:?}\nfull json: {json}"
    );
}
