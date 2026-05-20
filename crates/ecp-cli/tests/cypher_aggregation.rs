//! E2E tests for Cypher aggregation: COUNT, DISTINCT, and ORDER BY.
//!
//! Fixture: 4 functions with edges a->b, a2->b, b->c (3 Calls edges total).

use serde_json::Value;
use std::process::Command;

const SOURCE: &str = r#"
function a() { return b(); }
function a2() { return b(); }
function b() { return c(); }
function c() { return 1; }
"#;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn init_repo_and_analyze(repo: &std::path::Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/agg.ts"), SOURCE).unwrap();

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

fn run_json(repo: &std::path::Path, args: &[&str]) -> Value {
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

/// COUNT(*) per caller, ordered DESC by count then by name.
/// Edges: a->b, a2->b, b->c — each caller has exactly 1 outgoing call.
/// ORDER BY n DESC, a.name → 3 rows each with n=1.
#[test]
fn count_calls_per_caller() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (a:Function)-[:Calls]->(b:Function) RETURN a.name, COUNT(*) AS n ORDER BY n DESC, a.name",
            "--format",
            "json",
        ],
    );

    let columns = out["columns"]
        .as_array()
        .unwrap_or_else(|| panic!("expected columns array, got {out}"));
    let rows = out["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("expected rows array, got {out}"));

    let col_names: Vec<&str> = columns.iter().map(|c| c.as_str().unwrap()).collect();
    let name_col = col_names.iter().position(|&c| c == "a.name").unwrap();
    let n_col = col_names.iter().position(|&c| c == "n").unwrap();

    assert_eq!(rows.len(), 3, "expected 3 callers: {out}");

    let caller_names: Vec<&str> = rows.iter().map(|r| r[name_col].as_str().unwrap()).collect();
    for name in ["a", "a2", "b"] {
        assert!(
            caller_names.contains(&name),
            "expected caller {name} in {caller_names:?}"
        );
    }
    for row in rows {
        assert_eq!(
            row[n_col].as_i64(),
            Some(1),
            "each caller should have count=1: {row}"
        );
    }
}

/// DISTINCT callees: b and c are called (b by a and a2, c by b). 2 distinct.
#[test]
fn distinct_callees() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (a)-[:Calls]->(b) RETURN DISTINCT b.name",
            "--format",
            "json",
        ],
    );

    let columns = out["columns"]
        .as_array()
        .unwrap_or_else(|| panic!("expected columns array, got {out}"));
    let rows = out["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("expected rows array, got {out}"));

    let col_names: Vec<&str> = columns.iter().map(|c| c.as_str().unwrap()).collect();
    let b_col = col_names.iter().position(|&c| c == "b.name").unwrap();

    assert_eq!(rows.len(), 2, "expected 2 distinct callees: {out}");

    // Cypher JSON shape: single-column RETURN flattens rows to `["b","c"]`
    // (scalars); multi-column returns `[["b",1],["c",2]]`. Handle both.
    let callee_names: Vec<&str> = rows
        .iter()
        .map(|r| {
            r.as_str()
                .or_else(|| r.get(b_col).and_then(|v| v.as_str()))
                .unwrap()
        })
        .collect();
    assert!(
        callee_names.contains(&"b"),
        "b should be a callee: {callee_names:?}"
    );
    assert!(
        callee_names.contains(&"c"),
        "c should be a callee: {callee_names:?}"
    );
}

/// COUNT(DISTINCT a.name): 3 distinct callers (a, a2, b) → single row with n=3.
#[test]
fn count_distinct_callers() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (a)-[:Calls]->(b) RETURN COUNT(DISTINCT a.name) AS n",
            "--format",
            "json",
        ],
    );

    let columns = out["columns"]
        .as_array()
        .unwrap_or_else(|| panic!("expected columns array, got {out}"));
    let rows = out["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("expected rows array, got {out}"));

    let col_names: Vec<&str> = columns.iter().map(|c| c.as_str().unwrap()).collect();
    let n_col = col_names.iter().position(|&c| c == "n").unwrap();

    assert_eq!(rows.len(), 1, "expected single aggregate row: {out}");
    // Single-column COUNT returns scalar row, not array. Handle both shapes.
    let n_value = rows[0]
        .as_i64()
        .or_else(|| rows[0].get(n_col).and_then(|v| v.as_i64()));
    assert_eq!(
        n_value,
        Some(3),
        "COUNT(DISTINCT a.name) should be 3: {out}"
    );
}
