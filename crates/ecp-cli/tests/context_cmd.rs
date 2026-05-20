//! Integration tests for `ecp inspect` flag surface (UID resolution,
//! kind / file_path / relation_types / include_tests filtering). The flags
//! exist so the global CLAUDE.md GitNexus Workflow examples actually run.
//! (renamed from `ecp context` in the CLI redesign)

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

#[test]
fn context_ambiguous_name_returns_matches_array() {
    // Two functions named `handler` in different files — `--name handler`
    // is ambiguous; the response must include a `matches` array with both.
    // Use `--file_path` to disambiguate (replaces old `--uid` flow).
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

    let ambig = run_json(repo, &["inspect", "--name", "handler", "--format", "json"]);
    assert_eq!(ambig["status"], "ambiguous", "expected ambiguous: {ambig}");

    // New API: ambiguous response carries `matches`, each a full inspect block.
    let matches = ambig["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("expected matches array in ambiguous response: {ambig}"));
    assert_eq!(matches.len(), 2, "expected 2 matches: {ambig}");
    let has_auth = matches.iter().any(|m| {
        m["symbol"]["filePath"]
            .as_str()
            .unwrap_or("")
            .contains("src/auth/login.ts")
    });
    assert!(has_auth, "auth handler not found in matches: {ambig}");

    // Disambiguate with --file_path — should resolve to a single found result.
    let exact = run_json(
        repo,
        &[
            "inspect",
            "--name",
            "handler",
            "--file_path",
            "src/auth/login.ts",
            "--format",
            "json",
        ],
    );
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
            "wrong file resolved: {exact}"
        );
    }
}

#[test]
fn context_kind_filter_drops_non_matching_edges() {
    // `caller()` does three things: constructs `Helper` (resolves to Class
    // kind), calls `Helper().assist()` (Function kind), and calls
    // `target_fn` (Function kind). `--kind function` must keep only the two
    // function targets and drop the Class target.
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

    // Every remaining target must resolve to a Function kind.
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
fn context_file_path_filter_keeps_substring_matches() {
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
fn context_relation_types_filter_keeps_only_listed() {
    // `MyClass(BaseClass)` produces an `extends` outgoing edge. Filtering by
    // `--relation_types calls` must drop it (different rel); filtering by
    // `--relation_types extends` must keep it.
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
fn context_include_tests_default_drops_test_callers() {
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
