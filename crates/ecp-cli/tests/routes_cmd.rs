//! Integration tests for `ecp routes [<path>]`.
//!
//! Reuses the same TypeScript Express fixture as `api_impact_cmd.rs`:
//! two routes (`GET /api/users`, `POST /api/users`) each with a named
//! handler and an upstream caller function.

mod common;

use common::run_git;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
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

/// Run `ecp routes [extra...]` and return parsed JSON output.
fn run_routes_json(repo: &Path, home: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["routes", "--repo", ".", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(ecp_bin())
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

/// Run `ecp routes [extra...]` and return raw stdout as a string.
fn run_routes_stdout(repo: &Path, home: &Path, extra: &[&str]) -> String {
    let mut args = vec!["routes", "--repo", "."];
    args.extend_from_slice(extra);
    let out = Command::new(ecp_bin())
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

#[test]
fn routes_empty_includes_framework_hint() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_empty_repo(repo.path(), home.path());

    // Capture stderr as well — the hint goes to stderr.
    let args = ["routes", "--repo", ".", "--format", "json"];
    let out = Command::new(ecp_bin())
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
            || combined.contains("summary")
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

// The registration shares the enclosing function's last line.
export function register_tail() {
    app.get("/api/tail", list_users); }
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

/// The scope's end line is inclusive and 1-based like the route's line, so a
/// registration on the scope's last line is still inside it.
#[test]
fn test_routes_route_on_scope_last_line_has_enclosing_scope() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_inline_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/api/tail"]);
    let routes = json["routes"].as_array().expect("routes array missing");
    let scope_names: Vec<&str> = routes
        .iter()
        .filter_map(|r| r["enclosingScope"]["name"].as_str())
        .collect();
    assert_eq!(scope_names, ["register_tail"], "full json: {json}");
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

// ── Laravel route groups across files ───────────────────────────────────────
//
// The provider loads `routes/admin.php` under prefix `admin`, middleware
// `api` and the controller namespace; the route file nests its own groups.
// `ecp routes` must show the full path, the middleware outermost first, and
// the controller method as the handler, not `<inline>`.

const LARAVEL_FILES: &[(&str, &str)] = &[
    (
        "app/Providers/RouteServiceProvider.php",
        r#"<?php
namespace App\Providers;

use Illuminate\Foundation\Support\Providers\RouteServiceProvider as ServiceProvider;
use Illuminate\Support\Facades\Route;

class RouteServiceProvider extends ServiceProvider
{
    protected $namespace = 'App\Http\Controllers';

    protected function mapAdminRoutes()
    {
        Route::prefix('admin')
            ->middleware('api')
            ->namespace($this->namespace)
            ->group(base_path('routes/admin.php'));
    }
}
"#,
    ),
    (
        "routes/admin.php",
        r#"<?php

use Illuminate\Support\Facades\Route;

Route::get('/health', function () { return 'ok'; });

Route::post('/logintest', 'AwardController@logintest');

Route::group(['middleware' => 'auth:admin'], function () {
    Route::prefix('import')
        ->group(function () {
            Route::post('/award', 'AwardController@import');
        });
});
"#,
    ),
    (
        "app/Http/Controllers/AwardController.php",
        r#"<?php
namespace App\Http\Controllers;

class AwardController extends Controller
{
    public function import()
    {
        return $this->parseSheet();
    }

    private function parseSheet() {}
}
"#,
    ),
    (
        "app/Http/Controllers/Admin/AwardController.php",
        r#"<?php
namespace App\Http\Controllers\Admin;

class AwardController extends Controller
{
    public function import() {}
}
"#,
    ),
    (
        "routes/web.php",
        r#"<?php

use Illuminate\Support\Facades\Route;

Route::any('/ping', function () { return 'pong'; });
"#,
    ),
];

fn setup_laravel_repo(repo: &Path, home: &Path) {
    setup_laravel_repo_with(repo, home, &[]);
}

/// [`setup_laravel_repo`] with `files` written over the fixture's.
fn setup_laravel_repo_with(repo: &Path, home: &Path, files: &[(&str, &str)]) {
    for (path, src) in LARAVEL_FILES.iter().chain(files) {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, src).unwrap();
    }
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/routes-laravel-test.git",
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

#[test]
fn test_routes_laravel_included_route_lists_prefixed_path_and_middleware() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_laravel_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &[]);
    let rows: Vec<(&str, Option<&Value>)> = json["results"]
        .as_array()
        .expect("results array missing")
        .iter()
        .map(|r| (r["path"].as_str().unwrap_or(""), r.get("middleware")))
        .collect();
    let award = rows
        .iter()
        .find(|(path, _)| *path == "/admin/import/award")
        .unwrap_or_else(|| panic!("no /admin/import/award in {json}"));
    assert_eq!(
        award.1,
        Some(&serde_json::json!(["api", "auth:admin"])),
        "{json}"
    );
    let health = rows
        .iter()
        .find(|(path, _)| *path == "/admin/health")
        .unwrap_or_else(|| panic!("no /admin/health in {json}"));
    assert_eq!(health.1, Some(&serde_json::json!(["api"])), "{json}");
}

#[test]
fn test_routes_laravel_string_action_resolves_namespaced_controller_method() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_laravel_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["/admin/import/award"]);
    assert_eq!(json["status"], "found", "{json}");
    assert_eq!(
        json["routes"][0]["middleware"],
        serde_json::json!(["api", "auth:admin"]),
        "{json}"
    );
    let handlers: Vec<(&str, &str, &str)> = json["handlers"]
        .as_array()
        .expect("handlers array missing")
        .iter()
        .map(|h| {
            (
                h["handlerKind"].as_str().unwrap_or(""),
                h["name"].as_str().unwrap_or(""),
                h["filePath"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        handlers,
        [(
            "named",
            "import",
            "app/Http/Controllers/AwardController.php"
        )],
        "{json}"
    );
}

/// When no route has middleware, no row carries the `middleware` field.
#[test]
fn test_routes_route_without_middleware_omits_middleware_field() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &[]);
    for row in json["results"].as_array().unwrap() {
        assert!(row.get("middleware").is_none(), "{row}");
    }
}

/// When one route has middleware, every row carries the field, an empty
/// array for a route without any, so TOON keeps the rows one table.
#[test]
fn test_routes_list_mixed_middleware_gives_every_row_middleware_key() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_laravel_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &[]);
    let rows = json["results"].as_array().expect("results array missing");
    for row in rows {
        assert!(row.get("middleware").is_some(), "{row}");
    }
    let ping = rows
        .iter()
        .find(|row| row["path"] == "/ping")
        .unwrap_or_else(|| panic!("no /ping in {json}"));
    assert_eq!(ping["middleware"], serde_json::json!([]), "{json}");

    let toon = run_routes_stdout(repo.path(), home.path(), &[]);
    let header = toon
        .lines()
        .find(|line| line.trim_start().starts_with("results["))
        .unwrap_or_else(|| panic!("no results header in {toon}"));
    assert!(
        header.contains('{') && header.contains("middleware"),
        "results is not one table: {toon}"
    );
}

/// `(method, path)` of each row, sorted.
fn method_rows(json: &Value) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = json["results"]
        .as_array()
        .expect("results array missing")
        .iter()
        .map(|r| {
            (
                r["method"].as_str().unwrap_or("").to_string(),
                r["path"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    rows.sort_unstable();
    rows
}

/// A Laravel `Route::any` route is one `ANY` row, and a `--method` filter
/// for any verb Laravel registers it for keeps it, in any case.
#[test]
fn test_routes_method_filter_get_keeps_any_route() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_laravel_repo(repo.path(), home.path());

    for verb in ["GET", "get"] {
        let json = run_routes_json(repo.path(), home.path(), &["--method", verb]);
        let rows = method_rows(&json);
        let expected = [("ANY", "/ping"), ("GET", "/admin/health")]
            .map(|(m, p)| (m.to_string(), p.to_string()));
        assert_eq!(rows, expected, "{json}");
    }
}

/// `Route::any` registers GET HEAD POST PUT PATCH DELETE OPTIONS
/// (`Router::$verbs`), so a `TRACE` filter drops the `ANY` row.
#[test]
fn test_routes_method_filter_verb_any_never_registers_drops_any_route() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_laravel_repo(repo.path(), home.path());

    let json = run_routes_json(repo.path(), home.path(), &["--method", "TRACE"]);
    assert!(method_rows(&json).is_empty(), "{json}");
}

/// `(handlerKind, name, action)` of each handler of route `path`.
fn handler_kinds(repo: &Path, home: &Path, path: &str) -> Vec<(String, String, Option<String>)> {
    let json = run_routes_json(repo, home, &[path]);
    assert_eq!(json["status"], "found", "{json}");
    json["handlers"]
        .as_array()
        .expect("handlers array missing")
        .iter()
        .map(|h| {
            (
                h["handlerKind"].as_str().unwrap_or("").to_string(),
                h["name"].as_str().unwrap_or("").to_string(),
                h["action"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// A route whose string action names a method the controller lacks has no
/// handler edge, like a closure route, but it is no inline handler: `ecp
/// routes` reports the action as written, unresolved. A closure route stays
/// `inline_anonymous`.
#[test]
fn test_routes_unbound_string_action_reports_unresolved_not_inline() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_laravel_repo(repo.path(), home.path());

    assert_eq!(
        handler_kinds(repo.path(), home.path(), "/admin/logintest"),
        [(
            "unresolved".to_string(),
            "AwardController@logintest".to_string(),
            Some("AwardController@logintest".to_string()),
        )]
    );
    assert_eq!(
        handler_kinds(repo.path(), home.path(), "/admin/health"),
        [("inline_anonymous".to_string(), "<inline>".to_string(), None)]
    );
}

/// Two registrations of one route on one line: Laravel keeps the later one
/// (its route collection is keyed by verb and URI), so the route is one row
/// whose handler is the closure. Contract: the string action's BlindSpot never
/// marks the closure that replaced it, so the closure stays `inline_anonymous`.
#[test]
fn test_routes_same_route_action_then_closure_keeps_the_closure() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let admin = "<?php\n\nuse Illuminate\\Support\\Facades\\Route;\n\nRoute::get('/x', 'AwardController@missing'); Route::get('/x', function () { return 'ok'; });\n";
    setup_laravel_repo_with(repo.path(), home.path(), &[("routes/admin.php", admin)]);

    assert_eq!(
        handler_kinds(repo.path(), home.path(), "/admin/x"),
        [("inline_anonymous".to_string(), "<inline>".to_string(), None)]
    );
}
