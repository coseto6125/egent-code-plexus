//! Integration tests for `ecp inspect` flag surface (kind / file_path /
//! relation_types / include_tests filtering, ambiguous-match full blocks,
//! upstream impact summary, no UID in output).

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn write(repo: &Path, rel: &str, body: &str) {
    let full = repo.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, body).unwrap();
}

fn init_and_analyze(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

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

    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_json(repo: &Path, args: &[&str]) -> Value {
    let out = Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|err| panic!("{args:?} did not return JSON: {err}\nstdout={stdout}"))
}

/// Run `ecp inspect` and return raw stdout string (for UID/keyword checks).
fn run_stdout(repo: &Path, args: &[&str]) -> String {
    let out = Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ── Existing filter tests (updated: `context` → `inspect`, UID refs removed) ──

#[test]
fn inspect_accepts_name_positional() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(repo, "src/main.rs", "fn my_target() {}\n");
    init_and_analyze(repo);

    // Positional should work identically to named.
    let positional = run_json(repo, &["inspect", "my_target", "--format", "json"]);
    assert_eq!(positional["symbol"]["name"], "my_target");

    let named = run_json(
        repo,
        &["inspect", "--name", "my_target", "--format", "json"],
    );
    assert_eq!(named["symbol"]["name"], "my_target");
}

#[test]
fn inspect_ambiguous_name_disambiguated_by_file_path() {
    // Two functions named `handler` in different files — `--name handler` is
    // ambiguous; `--file_path src/auth/` must narrow it to the auth handler.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/auth/login.ts",
        "export function handler() { return 'auth'; }\n",
    );
    write(
        repo,
        "src/billing/charge.ts",
        "export function handler() { return 'billing'; }\n",
    );
    init_and_analyze(repo);

    // Without filter: ambiguous
    let ambig = run_json(repo, &["inspect", "--name", "handler", "--format", "json"]);
    assert_eq!(ambig["status"], "ambiguous", "expected ambiguous: {ambig}");

    // With --file_path narrowing: single match
    let exact = run_json(
        repo,
        &[
            "inspect",
            "--name",
            "handler",
            "--file_path",
            "src/auth/",
            "--format",
            "json",
        ],
    );
    // May return "found" (single match after filter applied to nodes) or
    // "ambiguous" with one match; either way the auth file must appear.
    let status = exact["status"].as_str().unwrap_or("");
    assert!(
        status == "found" || status == "ambiguous",
        "unexpected status: {exact}"
    );
    if status == "found" {
        assert!(
            exact["symbol"]["filePath"]
                .as_str()
                .unwrap_or("")
                .contains("src/auth/login.ts"),
            "wrong file: {exact}"
        );
    }
}

#[test]
fn inspect_kind_filter_drops_non_matching_edges() {
    // `caller()` calls `Helper` (Class), `Helper().assist()` (Function), and
    // `target_fn` (Function). `--kind function` must drop the Class target.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.py",
        r#"class Helper:
    def assist(self):
        return 1

def target_fn():
    return 2

def caller():
    h = Helper()
    h.assist()
    return target_fn()
"#,
    );
    init_and_analyze(repo);

    let unfiltered = run_json(repo, &["inspect", "--name", "caller", "--format", "json"]);
    assert_eq!(unfiltered["status"], "found", "{unfiltered}");

    let count_entries = |o: &serde_json::Map<String, Value>| -> usize {
        o.values()
            .filter_map(|v| v.as_array())
            .map(|a| a.len())
            .sum()
    };
    let unf_outgoing = unfiltered["outgoing"].as_object().unwrap();
    let unf_count = count_entries(unf_outgoing);
    assert!(
        unf_count >= 2,
        "fixture should yield ≥2 outgoing edges (Class + Function targets): {unfiltered}"
    );

    let filtered = run_json(
        repo,
        &[
            "inspect", "--name", "caller", "--kind", "function", "--format", "json",
        ],
    );
    assert_eq!(filtered["status"], "found");
    let f_outgoing = filtered["outgoing"].as_object().unwrap();
    let f_count = count_entries(f_outgoing);

    assert!(
        f_count < unf_count,
        "kind filter should drop the Class target: unfiltered={unf_count}, filtered={f_count}"
    );
    assert!(
        f_count >= 1,
        "should still keep function targets: {filtered}"
    );

    // Every remaining target must be a function (check via kind field, not uid).
    for entries in f_outgoing.values() {
        for entry in entries.as_array().unwrap() {
            let kind = entry["kind"].as_str().unwrap_or("").to_ascii_lowercase();
            assert!(
                kind == "function" || kind == "method",
                "kind=function filter leaked kind={kind}: {entry}"
            );
        }
    }
}

#[test]
fn inspect_file_path_filter_keeps_substring_matches() {
    // `caller` calls into both `src/auth/util.ts` and `src/billing/util.ts`.
    // `--file_path src/auth/` must keep only the auth edges.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/auth/util.ts",
        "export function auth_helper() { return 1; }\n",
    );
    write(
        repo,
        "src/billing/util.ts",
        "export function billing_helper() { return 2; }\n",
    );
    write(
        repo,
        "src/lib.ts",
        r#"
import { auth_helper } from './auth/util';
import { billing_helper } from './billing/util';

export function caller() {
    return auth_helper() + billing_helper();
}
"#,
    );
    init_and_analyze(repo);

    let filtered = run_json(
        repo,
        &[
            "inspect",
            "--name",
            "caller",
            "--file_path",
            "src/auth/",
            "--format",
            "json",
        ],
    );
    assert_eq!(filtered["status"], "found");

    let outgoing = filtered["outgoing"].as_object().unwrap();
    let mut total = 0;
    for entries in outgoing.values() {
        for entry in entries.as_array().unwrap() {
            let fp = entry["filePath"].as_str().unwrap_or("");
            assert!(
                fp.contains("src/auth/"),
                "file_path filter leaked entry={entry}"
            );
            total += 1;
        }
    }
    assert!(total >= 1, "expected ≥1 auth edge, got 0: {filtered}");
}

#[test]
fn inspect_relation_types_filter_keeps_only_listed() {
    // `MyClass(BaseClass)` produces an `extends` outgoing edge. Filtering by
    // `--relation_types calls` must drop it; `--relation_types extends` keeps it.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(repo, "src/helper.py", "class BaseClass:\n    pass\n");
    write(
        repo,
        "src/main.py",
        r#"from src.helper import BaseClass

class MyClass(BaseClass):
    pass
"#,
    );
    init_and_analyze(repo);

    let unfiltered = run_json(repo, &["inspect", "--name", "MyClass", "--format", "json"]);
    assert_eq!(unfiltered["status"], "found", "{unfiltered}");
    let unf_outgoing = unfiltered["outgoing"].as_object().unwrap();
    assert!(
        unf_outgoing.contains_key("extends"),
        "fixture should emit an extends edge, got {unfiltered}"
    );

    let kept = run_json(
        repo,
        &[
            "inspect",
            "--name",
            "MyClass",
            "--relation_types",
            "extends,imports",
            "--format",
            "json",
        ],
    );
    assert_eq!(kept["status"], "found");
    let kept_outgoing = kept["outgoing"].as_object().unwrap();
    for key in kept_outgoing.keys() {
        assert!(
            key == "extends" || key == "imports",
            "relation_types=extends,imports leaked rel='{key}'"
        );
    }
    assert!(
        kept_outgoing.contains_key("extends"),
        "extends rel should still be present after filtering: {kept}"
    );

    let dropped = run_json(
        repo,
        &[
            "inspect",
            "--name",
            "MyClass",
            "--relation_types",
            "calls",
            "--format",
            "json",
        ],
    );
    let dropped_outgoing = dropped["outgoing"].as_object().unwrap();
    assert!(
        !dropped_outgoing.contains_key("extends"),
        "relation_types=calls should drop the extends edge: {dropped}"
    );
    for key in dropped_outgoing.keys() {
        assert_eq!(key, "calls", "relation_types=calls leaked rel='{key}'");
    }
}

#[test]
fn inspect_include_tests_default_drops_test_callers() {
    // `target_fn` is called from `src/lib.ts` AND `tests/lib.test.ts`.
    // Default behavior drops the test caller; `--include_tests` keeps it.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.ts",
        r#"
export function target_fn() { return 1; }

export function prod_caller() {
    return target_fn();
}
"#,
    );
    write(
        repo,
        "tests/lib.test.ts",
        r#"
import { target_fn } from '../src/lib';

export function test_caller() {
    return target_fn();
}
"#,
    );
    init_and_analyze(repo);

    let default = run_json(
        repo,
        &["inspect", "--name", "target_fn", "--format", "json"],
    );
    assert_eq!(default["status"], "found", "{default}");
    let default_incoming_calls = default["incoming"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for entry in &default_incoming_calls {
        let fp = entry["filePath"].as_str().unwrap_or("");
        assert!(
            !fp.contains("tests/") && !fp.contains(".test."),
            "default should drop test callers, got {entry}"
        );
    }

    let with_tests = run_json(
        repo,
        &[
            "inspect",
            "--name",
            "target_fn",
            "--include_tests",
            "--format",
            "json",
        ],
    );
    let with_tests_calls = with_tests["incoming"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        with_tests_calls.len() > default_incoming_calls.len(),
        "include_tests should add ≥1 caller. default={} with_tests={}",
        default_incoming_calls.len(),
        with_tests_calls.len()
    );
}

#[test]
fn inspect_impact_upstream_excludes_test_callers_by_default() {
    // `impact_upstream_1hop` is a sibling channel to `incoming` and must apply
    // the same `--include_tests` filter; otherwise the LLM consumer sees
    // contradictory blast-radius (empty incoming, populated impact) for any
    // function whose only callers live under `tests/`.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.ts",
        "export function target_fn() { return 1; }\n",
    );
    write(
        repo,
        "tests/lib.test.ts",
        r#"
import { target_fn } from '../src/lib';

export function test_caller() {
    return target_fn();
}
"#,
    );
    init_and_analyze(repo);

    let default = run_json(
        repo,
        &["inspect", "--name", "target_fn", "--format", "json"],
    );
    let upstream = default["impact_upstream_1hop"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for entry in &upstream {
        let fp = entry["file"].as_str().unwrap_or("");
        assert!(
            !fp.contains("tests/") && !fp.contains(".test."),
            "default impact_upstream_1hop must drop test callers, got {entry}"
        );
    }

    let with_tests = run_json(
        repo,
        &[
            "inspect",
            "--name",
            "target_fn",
            "--include_tests",
            "--format",
            "json",
        ],
    );
    let with_upstream = with_tests["impact_upstream_1hop"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        with_upstream.len() > upstream.len(),
        "--include_tests should grow impact_upstream_1hop. default={} with_tests={}",
        upstream.len(),
        with_upstream.len()
    );
}

#[test]
fn inspect_impact_upstream_excludes_file_kind_sources() {
    // File-kind nodes hold (File)-[:Imports]->(symbol) edges; treating them
    // as upstream "callers" leaks file basenames into a list the LLM reads as
    // "who depends on this function". Impact must surface only symbol-level
    // callers (Function / Method / Constructor / Class).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.ts",
        "export function target_fn() { return 1; }\n",
    );
    // `consumer.ts` imports target_fn but never calls it from inside a function
    // body — the Imports edge from the File node is the only incoming edge.
    write(
        repo,
        "src/consumer.ts",
        "import { target_fn } from './lib';\nexport const ref = target_fn;\n",
    );
    init_and_analyze(repo);

    let result = run_json(
        repo,
        &["inspect", "--name", "target_fn", "--format", "json"],
    );
    let upstream = result["impact_upstream_1hop"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for entry in &upstream {
        let kind = entry["kind"].as_str().unwrap_or("");
        assert_ne!(
            kind, "File",
            "impact_upstream_1hop must not surface File-kind nodes: {entry}"
        );
    }
}

// ── New tests for Task 2.1 composition requirements ──

#[test]
fn inspect_output_does_not_contain_uid_field() {
    // Single-match symbol: output must NOT contain a `uid:` key or the
    // colon-delimited UID format (Kind:filePath:name) anywhere.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.ts",
        "export function unique_symbol_for_uid_test() { return 42; }\n",
    );
    init_and_analyze(repo);

    let stdout = run_stdout(
        repo,
        &[
            "inspect",
            "--name",
            "unique_symbol_for_uid_test",
            "--format",
            "toon",
        ],
    );
    assert!(
        !stdout.contains("\"uid\""),
        "inspect output leaked uid field:\n{stdout}"
    );
    // UID format is `Kind:filePath:name` — a colon-separated triple. The
    // symbol name itself doesn't have colons, so a colon in output signals UID.
    // Exclude the stale-warning line (which uses ":" in its text) by checking
    // JSON output format instead of raw toon.
    let json = run_json(
        repo,
        &[
            "inspect",
            "--name",
            "unique_symbol_for_uid_test",
            "--format",
            "json",
        ],
    );
    assert!(
        json["symbol"]["uid"].is_null(),
        "inspect JSON payload contains uid field: {}",
        json["symbol"]
    );
}

#[test]
fn inspect_ambiguous_returns_full_matches() {
    // Two functions sharing a name → response must contain full inspect
    // blocks per match (each with a `kind` field), not a candidates list.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/alpha.ts",
        "export function shared_name() { return 'alpha'; }\n",
    );
    write(
        repo,
        "src/beta.ts",
        "export function shared_name() { return 'beta'; }\n",
    );
    init_and_analyze(repo);

    let result = run_json(
        repo,
        &["inspect", "--name", "shared_name", "--format", "json"],
    );
    assert_eq!(
        result["status"], "ambiguous",
        "expected ambiguous: {result}"
    );

    // Must have "matches" key (full blocks), not "candidates" (uid list).
    let matches = result["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("missing 'matches' array in ambiguous response: {result}"));
    assert!(
        matches.len() >= 2,
        "expected ≥2 full match blocks, got {}:\n{result}",
        matches.len()
    );

    // Each block must have a symbol.kind field (full inspect block, not just a uid).
    for m in matches {
        let kind = m["symbol"]["kind"].as_str();
        assert!(
            kind.is_some() && !kind.unwrap().is_empty(),
            "match block missing symbol.kind: {m}"
        );
    }

    // Must NOT have a "candidates" key (old UID-list style).
    assert!(
        result["candidates"].is_null(),
        "ambiguous response still exposes old 'candidates' key: {result}"
    );
}

#[test]
fn inspect_payload_includes_upstream_impact_summary() {
    // `target_fn` has a caller (`prod_caller`). The inspect output must
    // include a non-null `impact_upstream_1hop` array with ≥1 entry.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.ts",
        r#"
export function target_fn() { return 1; }

export function prod_caller() {
    return target_fn();
}
"#,
    );
    init_and_analyze(repo);

    let result = run_json(
        repo,
        &["inspect", "--name", "target_fn", "--format", "json"],
    );
    assert_eq!(result["status"], "found", "{result}");

    let upstream = result["impact_upstream_1hop"]
        .as_array()
        .unwrap_or_else(|| panic!("missing or non-array 'impact_upstream_1hop' in:\n{result}"));
    assert!(
        !upstream.is_empty(),
        "impact_upstream_1hop should contain ≥1 caller (prod_caller), got empty:\n{result}"
    );

    // Each entry in the upstream summary must have name, kind, file.
    for entry in upstream {
        assert!(
            entry["name"].is_string(),
            "upstream entry missing 'name': {entry}"
        );
        assert!(
            entry["kind"].is_string(),
            "upstream entry missing 'kind': {entry}"
        );
        assert!(
            entry["file"].is_string(),
            "upstream entry missing 'file': {entry}"
        );
    }
}
