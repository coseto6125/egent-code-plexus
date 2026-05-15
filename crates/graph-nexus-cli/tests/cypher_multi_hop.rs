//! E2E tests for multi-hop Cypher patterns: chained MATCH, variable-length
//! paths, and reverse-arrow traversal.

use serde_json::Value;
use std::process::Command;

// Three functions forming a linear call chain: a -> b -> c.
const SOURCE: &str = r#"
function a() { return b(); }
function b() { return c(); }
function c() { return 1; }
"#;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn init_repo_and_analyze(repo: &std::path::Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/chain.ts"), SOURCE).unwrap();

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
    let out = Command::new(gnx_bin())
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

/// Two-hop chain: a->b->c should produce exactly one row ["a", "b", "c"].
#[test]
fn multi_hop_chain() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (a:Function)-[:Calls]->(b:Function)-[:Calls]->(c:Function) RETURN a.name, b.name, c.name",
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
    let a_col = col_names.iter().position(|&c| c == "a.name").unwrap();
    let b_col = col_names.iter().position(|&c| c == "b.name").unwrap();
    let c_col = col_names.iter().position(|&c| c == "c.name").unwrap();

    assert_eq!(rows.len(), 1, "expected exactly 1 two-hop path: {out}");
    assert_eq!(rows[0][a_col].as_str(), Some("a"), "a.name mismatch: {out}");
    assert_eq!(rows[0][b_col].as_str(), Some("b"), "b.name mismatch: {out}");
    assert_eq!(rows[0][c_col].as_str(), Some("c"), "c.name mismatch: {out}");
}

/// Variable-length path *1..2 expands BFS up to depth 2.
/// Expected pairs: (a,b), (b,c), (a,c) — 3 rows when ordered.
#[test]
fn variable_length_path() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (a:Function)-[:Calls*1..2]->(b:Function) RETURN DISTINCT a.name, b.name ORDER BY a.name, b.name",
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
    let a_col = col_names.iter().position(|&c| c == "a.name").unwrap();
    let b_col = col_names.iter().position(|&c| c == "b.name").unwrap();

    // Collect (a.name, b.name) pairs.
    let pairs: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r[a_col].as_str().unwrap(),
                r[b_col].as_str().unwrap(),
            )
        })
        .collect();

    // Must contain direct edges and the depth-2 hop a->c.
    assert!(pairs.contains(&("a", "b")), "missing a->b: {pairs:?}");
    assert!(pairs.contains(&("b", "c")), "missing b->c: {pairs:?}");
    assert!(pairs.contains(&("a", "c")), "missing a->c (depth-2): {pairs:?}");
    assert_eq!(pairs.len(), 3, "expected 3 distinct pairs: {pairs:?}");
}

/// Reverse arrow `<-[:Calls]-` should find the same edges from the target side.
/// With chain a->b->c, `(a)<-[:Calls]-(b)` must yield rows where b calls a.
/// Here: b calls a is not in the chain (a calls b, b calls c); so we match
/// from the callee perspective: `(x)<-[:Calls]-(y)` means y->x edge exists.
/// Pairs: (b,a) and (c,b) — both callees on the left, callers on the right.
#[test]
fn reverse_arrow() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (callee:Function)<-[:Calls]-(caller:Function) RETURN callee.name, caller.name ORDER BY caller.name",
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
    let callee_col = col_names.iter().position(|&c| c == "callee.name").unwrap();
    let caller_col = col_names.iter().position(|&c| c == "caller.name").unwrap();

    assert_eq!(rows.len(), 2, "expected 2 reverse edges: {out}");

    let pairs: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r[callee_col].as_str().unwrap(), r[caller_col].as_str().unwrap()))
        .collect();

    // a is called by nobody (a is the root); b is called by a; c is called by b.
    assert!(pairs.contains(&("b", "a")), "b should be called by a: {pairs:?}");
    assert!(pairs.contains(&("c", "b")), "c should be called by b: {pairs:?}");
}
