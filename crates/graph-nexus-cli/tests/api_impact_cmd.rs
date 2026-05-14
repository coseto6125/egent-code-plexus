//! Integration tests for `gnx api_impact --route <path>`.
//!
//! Fixture: a tiny FastAPI module with two routes (`GET /api/users` and
//! `POST /api/users`), each backed by a handler function. A separate
//! `caller_*` function calls into each handler so we can assert that the
//! upstream BFS surfaces at least one caller.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

// TypeScript Express fixture — chosen because the upstream route_detector
// emits Route nodes cleanly for `app.METHOD(path, handler)` form. Python
// FastAPI's `@app.get("/x")` decorator currently does NOT emit Route nodes
// (separate bug: route_detector::detect_from_call doesn't strip quotes
// off the path string when the path comes from a Python tree-sitter
// `string` node). Using Express here keeps api_impact tests focused on
// the command's own logic.
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
            "git@github.com:E-NoR/api-impact-test.git",
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

fn run_api_impact(repo: &Path, home: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["api_impact", "--repo", ".", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(gnx_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("api_impact spawn failed");
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

#[test]
fn api_impact_finds_route_handler_and_callers() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_api_impact(repo.path(), home.path(), &["--route", "/api/users"]);

    assert_eq!(json["status"], "found", "expected status=found: {json}");

    let routes = json["routes"].as_array().expect("routes array missing");
    assert!(
        !routes.is_empty(),
        "expected ≥1 route match for /api/users: {json}"
    );

    let handlers = json["handlers"].as_array().expect("handlers array missing");
    let handler_names: Vec<&str> = handlers
        .iter()
        .map(|h| h["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        handler_names.contains(&"list_users") || handler_names.contains(&"create_user"),
        "expected list_users / create_user in handlers, got: {:?}",
        handler_names
    );

    let callers = json["callers"].as_array().expect("callers array missing");
    assert!(
        !callers.is_empty(),
        "expected ≥1 upstream caller for /api/users handlers: {json}"
    );
    let caller_names: Vec<&str> = callers
        .iter()
        .map(|c| c["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        caller_names
            .iter()
            .any(|n| n.starts_with("caller_") || n.starts_with("_")),
        "expected upstream caller_* / _build_*  in callers, got: {:?}",
        caller_names
    );
}

#[test]
fn api_impact_not_found_for_unknown_route() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    let json = run_api_impact(repo.path(), home.path(), &["--route", "/no-such-path"]);
    assert_eq!(
        json["status"], "not_found",
        "expected status=not_found for unknown route: {json}"
    );
    assert_eq!(json["route_pattern"], "/no-such-path");
    // candidates may be empty if no routes share any prefix — only assert key.
    assert!(
        json.get("candidates").is_some(),
        "candidates key missing: {json}"
    );
}

#[test]
fn api_impact_method_filter_disambiguates_same_path() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());

    // Without --method: both GET and POST handlers should surface.
    let both = run_api_impact(repo.path(), home.path(), &["--route", "/api/users"]);
    let both_handler_names: Vec<&str> = both["handlers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        both_handler_names.contains(&"list_users") && both_handler_names.contains(&"create_user"),
        "expected both GET+POST handlers without --method, got: {:?}",
        both_handler_names
    );

    // --method POST: only the create_user handler should surface.
    let only_post = run_api_impact(
        repo.path(),
        home.path(),
        &["--route", "/api/users", "--method", "POST"],
    );
    assert_eq!(only_post["status"], "found");
    let post_handler_names: Vec<&str> = only_post["handlers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        post_handler_names.contains(&"create_user"),
        "expected create_user in --method POST result, got: {:?}",
        post_handler_names
    );
    assert!(
        !post_handler_names.contains(&"list_users"),
        "list_users (GET) leaked into --method POST result: {:?}",
        post_handler_names
    );

    // Routes list should likewise be filtered down to a single POST.
    let routes = only_post["routes"].as_array().unwrap();
    for r in routes {
        assert_eq!(
            r["method"].as_str().unwrap_or(""),
            "POST",
            "non-POST route leaked: {r}"
        );
    }
}
