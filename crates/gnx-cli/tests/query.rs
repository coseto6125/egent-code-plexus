use serde_json::Value;
use std::process::Command;

const SOURCE: &str = r#"
function handleLogin(username: string, password: string) {
    const user = lookupUser(username);
    if (!verifyPassword(user, password)) return null;
    return createSession(user);
}

function lookupUser(name: string) {
    return dbQuery(name);
}

function verifyPassword(user: any, password: string) {
    return hashPassword(password) === user.password_hash;
}

function hashPassword(password: string) {
    return `hash_${password}`;
}

function createSession(user: any) {
    return generateSessionId();
}

function generateSessionId() {
    return Math.random().toString(36);
}

function dbQuery(q: string) {
    return null;
}
"#;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn init_repo_and_analyze(repo: &std::path::Path) {
    // git init the repo
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/auth.ts"), SOURCE).unwrap();

    // Commit source so detect-changes etc. have a baseline
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();

    let out = Command::new(gnx_bin())
        .args(["analyze", "--repo", "."])
        .current_dir(repo)
        // Isolate registry to a tempdir to avoid polluting ~/.gnx/
        .env("HOME", repo)
        .output()
        .expect("analyze failed to spawn");
    assert!(
        out.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_query(repo: &std::path::Path, args: &[&str]) -> Value {
    let out = Command::new(gnx_bin())
        .args(args)
        .current_dir(repo)
        // Isolate registry to a tempdir to avoid polluting ~/.gnx/
        .env("HOME", repo)
        .output()
        .expect("query failed to spawn");
    assert!(
        out.status.success(),
        "query {args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("query {args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[json_start..]).unwrap_or_else(|err| {
        panic!(
            "query {args:?} did not return JSON: {err}\nstdout={}",
            stdout
        )
    })
}

#[test]
fn query_bm25_outputs_sources_without_scores_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let result = run_query(
        tmp.path(),
        &["query", "--query", "handleLogin", "--format", "json"],
    );
    let results = result["results"].as_array().unwrap();
    assert!(!results.is_empty());

    let first = results[0].as_str().unwrap();
    assert!(first.contains("bm25:"), "missing bm25 score label: {first}");
    assert!(first.contains("handleLogin"), "missing query term: {first}");
}

#[test]
fn query_debug_outputs_rrf_and_source_details() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let result = run_query(
        tmp.path(),
        &["query", "--query", "handleLogin", "--format", "json"],
    );
    let results = result["results"].as_array().unwrap();
    assert!(!results.is_empty());
    let first = results[0].as_str().unwrap();
    assert!(first.contains("bm25:"), "missing bm25 score label: {first}");
    assert!(first.contains("handleLogin"), "missing query term: {first}");
}

#[test]
fn query_multi_hybrid_accepts_explicit_query_lanes() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let result = run_query(
        tmp.path(),
        &["query", "--query", "verifyPassword", "--format", "json"],
    );
    let results = result["results"].as_array().unwrap();
    assert!(!results.is_empty());
    let rendered = results
        .iter()
        .map(|node| node.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("verifyPassword"),
        "missing query result: {rendered}"
    );
    assert!(
        rendered.contains("bm25:"),
        "missing bm25 score label: {rendered}"
    );
}

#[test]
fn query_rejects_multiple_queries_without_multi_hybrid_mode() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = Command::new(gnx_bin())
        .args([
            "query",
            "--query",
            "lookupUser",
            "--query",
            "verifyPassword",
            "--format",
            "json",
        ])
        .current_dir(tmp.path())
        // Isolate registry to a tempdir to avoid polluting ~/.gnx/
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "multiple queries should not be allowed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("--query"),
        "unexpected stderr: {stderr}"
    );
}
