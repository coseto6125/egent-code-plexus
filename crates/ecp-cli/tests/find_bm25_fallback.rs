//! F1: an exact `find` miss on a pattern that is no symbol name (whitespace,
//! punctuation) answers with bm25 over the same pattern, and says so with
//! `mode: "bm25"` and `fallback_from: "exact"`. A miss on a plain identifier
//! stays a trustworthy `found: false`.
//!
//! F3: `find` prints its caveat after the result rows in every format.

use crate::common;

use common::{commit_all, ecp_bin, run_git};
use std::path::Path;
use std::process::{Command, Output};

const SESSION_KEYS: [&str; 5] = [
    "ECP_SESSION_ID",
    "GEMINI_CLI_SESSION_ID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
];

fn run_ecp(cwd: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1");
    for key in SESSION_KEYS {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("ecp failed to spawn")
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("lib.rs"), "pub fn alpha() {}\n").unwrap();
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    commit_all(repo.path(), "init");
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["admin", "index", "--repo", "."],
        &[],
    );
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (repo, home)
}

/// The whole stdout as one JSON value: `--format json` prints nothing else.
fn json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "find failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON value ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn find_json(repo: &Path, home: &Path, pattern: &str) -> serde_json::Value {
    json(&run_ecp(
        repo,
        home,
        &["find", pattern, "--format", "json"],
        &[],
    ))
}

fn bucket_names(payload: &serde_json::Value) -> Vec<String> {
    [
        "source",
        "examples",
        "tests",
        "reference",
        "document",
        "config",
    ]
    .iter()
    .flat_map(|b| payload[*b].as_array().cloned().unwrap_or_default())
    .filter_map(|hit| hit["name"].as_str().map(str::to_owned))
    .collect()
}

/// Failed before F1: `alpha beta` is no declaration name, so the exact
/// lookup answered `found: false` and the agent re-ran with `--mode bm25`.
#[test]
fn test_find_exact_miss_whitespace_pattern_returns_bm25_fallback() {
    let (repo, home) = fixture();
    for pattern in ["alpha beta", "fn alpha"] {
        let payload = find_json(repo.path(), home.path(), pattern);
        assert_eq!(payload["mode"], "bm25", "{pattern}: {payload}");
        assert_eq!(payload["fallback_from"], "exact", "{pattern}: {payload}");
        assert!(
            payload.get("found").is_none(),
            "{pattern}: a bm25 payload has no exact `found`: {payload}"
        );
        assert!(
            bucket_names(&payload).iter().any(|n| n == "alpha"),
            "{pattern}: bm25 over the same text must find alpha: {payload}"
        );
    }
}

/// A miss on a plain identifier is the answer "no declaration has this
/// name": no fallback may blur it.
#[test]
fn test_find_exact_miss_identifier_returns_found_false_without_fallback() {
    let (repo, home) = fixture();
    let payload = find_json(repo.path(), home.path(), "no_such_symbol_zz");
    assert_eq!(payload["found"], false, "{payload}");
    assert_eq!(payload["status"], "success", "{payload}");
    assert!(payload.get("fallback_from").is_none(), "{payload}");
    assert!(payload.get("mode").is_none(), "{payload}");
}

/// Failed before: punctuation alone fell back, so a miss on a name that
/// Ruby (`save!`, `empty?`), JS (`#priv`) or CSS and HCL (`kebab-name`)
/// declare gave bm25 rows and no `found` key.
#[test]
fn test_find_exact_miss_punctuated_names_return_found_false_without_fallback() {
    let (repo, home) = fixture();
    for pattern in ["save!", "empty?", "#priv", "kebab-name"] {
        let payload = find_json(repo.path(), home.path(), pattern);
        assert_eq!(payload["found"], false, "{pattern}: {payload}");
        assert_eq!(payload["status"], "success", "{pattern}: {payload}");
        assert!(
            payload.get("fallback_from").is_none(),
            "{pattern}: {payload}"
        );
        assert!(payload.get("mode").is_none(), "{pattern}: {payload}");
    }
}

#[test]
fn test_find_exact_hit_returns_match_without_fallback() {
    let (repo, home) = fixture();
    let payload = find_json(repo.path(), home.path(), "alpha");
    assert_eq!(payload["found"], true, "{payload}");
    assert_eq!(payload["matches"][0]["name"], "alpha", "{payload}");
    assert_eq!(payload["matches"][0]["file"], "lib.rs", "{payload}");
    assert!(payload.get("fallback_from").is_none(), "{payload}");
}

/// The fallback that also finds nothing still says it ran, and keeps the
/// bm25 empty shape with its hint.
#[test]
fn test_find_exact_miss_fallback_bm25_empty_reports_fallback_and_hint() {
    let (repo, home) = fixture();
    let payload = find_json(repo.path(), home.path(), "zzq qqz");
    assert_eq!(payload["mode"], "bm25", "{payload}");
    assert_eq!(payload["fallback_from"], "exact", "{payload}");
    assert!(bucket_names(&payload).is_empty(), "{payload}");
    assert!(payload["hint"].as_str().is_some(), "{payload}");
}

/// An empty or whitespace-only pattern is no query: it fails, and no bm25
/// runs over nothing.
#[test]
fn test_find_exact_empty_or_blank_pattern_fails_without_fallback() {
    let (repo, home) = fixture();
    for pattern in ["", "   "] {
        let out = run_ecp(
            repo.path(),
            home.path(),
            &["find", pattern, "--format", "json"],
            &[],
        );
        assert!(!out.status.success(), "{pattern:?} must fail");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("fallback_from"),
            "{pattern:?}: no fallback payload: {stdout}"
        );
    }
}

/// The text form names the fallback before the rows.
#[test]
fn test_find_exact_miss_fallback_text_names_fallback() {
    let (repo, home) = fixture();
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["find", "alpha beta", "--format", "text"],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().unwrap_or_default();
    assert!(
        first.contains("bm25") && first.contains("fallback_from: exact"),
        "{stdout}"
    );
    assert!(stdout.contains("(alpha)"), "{stdout}");
}

// ── F3: the caveat follows the rows ─────────────────────────────────────────

const WARM_ATTACH_NOTE: &str = "warm-attach from a sibling commit";

/// A warm attach the overlay may not cover (limit 0): the staleness line
/// stays on every answer, whatever files the rows name.
fn uncovered_warm_attach(repo: &Path) {
    std::fs::write(repo.join("extra.rs"), "pub fn beta() {}\n").unwrap();
    commit_all(repo, "advance");
}

const UNCOVERED: [(&str, &str); 1] = [("ECP_WARM_ATTACH_MAX_FILES", "0")];

fn find_out(repo: &Path, home: &Path, args: &[&str]) -> String {
    let out = run_ecp(repo, home, args, &UNCOVERED);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Failed before F3: text printed the caveat as the first stdout line, so
/// `head -N` kept the caveat and cut the rows.
#[test]
fn test_find_exact_caveat_after_rows_in_json_llm_and_text() {
    let (repo, home) = fixture();
    uncovered_warm_attach(repo.path());

    let stdout = find_out(
        repo.path(),
        home.path(),
        &["find", "alpha", "--format", "json"],
    );
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        payload["result"]
            .as_str()
            .is_some_and(|r| r.contains(WARM_ATTACH_NOTE)),
        "the staleness line stays whatever the rows: {payload}"
    );
    let rows = stdout.find("\"matches\"").expect("rows");
    let caveat = stdout.find("\"result\"").expect("caveat");
    assert!(rows < caveat, "json: caveat after rows: {stdout}");

    let stdout = find_out(repo.path(), home.path(), &["find", "alpha"]);
    let rows = stdout.find("matches").expect("rows");
    let caveat = stdout.find("result").expect("caveat");
    assert!(rows < caveat, "llm: caveat after rows: {stdout}");

    let stdout = find_out(
        repo.path(),
        home.path(),
        &["find", "alpha", "--format", "text"],
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].contains("lib.rs:1"), "text: rows first: {stdout}");
    assert!(
        lines.last().is_some_and(|l| l.contains(WARM_ATTACH_NOTE)),
        "text: caveat last: {stdout}"
    );
}

#[test]
fn test_find_bm25_caveat_after_rows_in_text() {
    let (repo, home) = fixture();
    uncovered_warm_attach(repo.path());
    let stdout = find_out(
        repo.path(),
        home.path(),
        &["find", "alpha", "--mode", "bm25", "--format", "text"],
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "=== source ===", "text: rows first: {stdout}");
    assert!(
        lines.last().is_some_and(|l| l.contains(WARM_ATTACH_NOTE)),
        "text: caveat last: {stdout}"
    );
}

/// Failed before: the prose fallback returned before the unverified check, so
/// an uncovered warm attach answered `status: "success"` bm25 rows with no
/// `found` key, though HEAD may hold a declaration the exact lookup missed.
#[test]
fn test_find_exact_miss_prose_pattern_uncovered_warm_attach_returns_unverified_bm25() {
    let (repo, home) = fixture();
    uncovered_warm_attach(repo.path());

    let stdout = find_out(
        repo.path(),
        home.path(),
        &["find", "alpha beta", "--format", "json"],
    );
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(payload["mode"], "bm25", "{payload}");
    assert_eq!(payload["fallback_from"], "exact", "{payload}");
    assert!(
        payload.get("found").is_some_and(serde_json::Value::is_null),
        "found must be present and null: {payload}"
    );
    assert_eq!(payload["status"], "unverified", "{payload}");
    assert!(
        payload["reason"]
            .as_str()
            .is_some_and(|r| r.contains("limit")),
        "{payload}"
    );
    assert_eq!(
        payload["next"], "ecp admin index --force --repo .",
        "{payload}"
    );
    assert!(
        bucket_names(&payload).iter().any(|n| n == "alpha"),
        "bm25 still runs: {payload}"
    );

    let stdout = find_out(
        repo.path(),
        home.path(),
        &["find", "alpha beta", "--format", "text"],
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].contains("fallback_from: exact"), "{stdout}");
    assert!(
        lines[1].starts_with("unverified") && lines[1].contains("--force"),
        "{stdout}"
    );
}
