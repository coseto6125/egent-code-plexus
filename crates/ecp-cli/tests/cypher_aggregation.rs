//! E2E tests for Cypher aggregation: COUNT, DISTINCT, and ORDER BY.
//!
//! Fixture: 4 functions with edges a->b, a2->b, b->c (3 Calls edges total).
//! The `ownerClass` / `lexicalParent` tests index their own nested-scope
//! fixture ([`NESTED_FILES`]).

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
    init_repo_with_files(repo, &[("src/agg.ts", SOURCE)]);
}

/// A git repo at `repo` holding `files` (relative path, text), committed
/// and indexed.
fn init_repo_with_files(repo: &std::path::Path, files: &[(&str, &str)]) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    for (rel, text) in files {
        let path = repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

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

// ── ownerClass is the stored owner; lexicalParent the display chain ─────────
// FU-2026-10-05-ff801e095d23 (a)(b): `n.ownerClass` reads the owner stored on
// the node, `n.lexicalParent` reads `display_owner` (the enclosing-callable
// chain of a scoped node, else the stored owner).

/// Nested scopes in the languages whose nodes the edge states need: scoped
/// (Python, TypeScript, Rust) and unscoped (Dart, Kotlin, Swift).
const NESTED_FILES: &[(&str, &str)] = &[
    (
        "a.py",
        r#"def top():
    return 0


def outer():
    def inner():
        def deep():
            return 1
        return deep()
    return inner()


def maker():
    class Fake:
        pass
    return Fake()


class K:
    def m(self):
        def helper():
            return 2
        return helper()
"#,
    ),
    (
        "a.ts",
        r#"function tsTop(): number { return 0; }

class TK {
  m(): number {
    function tsInner(): number { return 1; }
    return tsInner();
  }
}
"#,
    ),
    (
        "b.rs",
        r#"struct A;

impl A {
    fn new() -> A {
        fn rs_inner() -> i32 { 1 }
        rs_inner();
        A
    }
}
"#,
    ),
    (
        "c.dart",
        "int dartOuter() {\n  int dartInner() { return 2; }\n  return dartInner();\n}\n",
    ),
    (
        "c.kt",
        "fun ktOuter(): Int {\n    fun ktInner(): Int { return 2 }\n    return ktInner()\n}\n",
    ),
    (
        "c.swift",
        "func swOuter() -> Int {\n    func swInner() -> Int { return 2 }\n    return swInner()\n}\n",
    ),
];

/// The `rows` of a cypher query on the indexed `NESTED_FILES`, in json.
fn nested_rows(repo: &std::path::Path, query: &str) -> Vec<Value> {
    let out = run_json(repo, &["cypher", query, "--format", "json"]);
    out["rows"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("expected rows array, got {out}"))
}

/// `[n.ownerClass, n.lexicalParent]` of the one node named `name` in `file`.
fn owner_row(repo: &std::path::Path, file: &str, name: &str) -> Value {
    let rows = nested_rows(
        repo,
        &format!(
            "MATCH (n) WHERE n.filePath = '{file}' AND n.name = '{name}' \
             RETURN n.ownerClass, n.lexicalParent"
        ),
    );
    assert_eq!(rows.len(), 1, "one {file}:{name} node: {rows:?}");
    rows[0].clone()
}

fn indexed_nested() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_files(tmp.path(), NESTED_FILES);
    tmp
}

/// Stands for: a closure `inner` in `def outer()` has no stored owner, so
/// `ownerClass` is null, and its enclosing fn is `lexicalParent`. Failed
/// before: executor.rs `node_prop_value` printed `display_owner` as
/// `ownerClass` (`"outer"`), and `n.lexicalParent` was an unknown property
/// that failed the query.
#[test]
fn test_node_prop_value_python_closure_returns_null_and_lexical_parent_outer() {
    let tmp = indexed_nested();
    assert_eq!(
        owner_row(tmp.path(), "a.py", "inner"),
        serde_json::json!([null, "outer"])
    );
}

/// Stands for: a nested fn at depth 2 prints the whole enclosing chain.
#[test]
fn test_node_prop_value_python_nested_depth_two_returns_chain() {
    let tmp = indexed_nested();
    assert_eq!(
        owner_row(tmp.path(), "a.py", "deep"),
        serde_json::json!([null, "outer::inner"])
    );
}

/// Stands for: a nested fn in a method has no stored owner; its
/// `lexicalParent` is the method's class and name. The method itself keeps
/// its class as `ownerClass`, and its `lexicalParent` is the display owner,
/// which for an unscoped node is the stored owner.
#[test]
fn test_node_prop_value_closure_in_method_returns_null_and_class_method_chain() {
    let tmp = indexed_nested();
    let path = tmp.path();
    assert_eq!(
        owner_row(path, "a.py", "helper"),
        serde_json::json!([null, "K::m"])
    );
    assert_eq!(owner_row(path, "a.py", "m"), serde_json::json!(["K", "K"]));
    assert_eq!(
        owner_row(path, "a.ts", "tsInner"),
        serde_json::json!([null, "TK::m"])
    );
    assert_eq!(
        owner_row(path, "a.ts", "m"),
        serde_json::json!(["TK", "TK"])
    );
}

/// Stands for: a Rust fn nested in `impl A { fn new }` has no stored owner
/// and the `lexicalParent` `A::new`; `new` keeps the owner `A`.
#[test]
fn test_node_prop_value_rust_nested_fn_in_impl_method_returns_null_and_chain() {
    let tmp = indexed_nested();
    let path = tmp.path();
    assert_eq!(
        owner_row(path, "b.rs", "rs_inner"),
        serde_json::json!([null, "A::new"])
    );
    assert_eq!(
        owner_row(path, "b.rs", "new"),
        serde_json::json!(["A", "A"])
    );
}

/// Stands for: a file-level fn has neither an owner nor a lexical parent.
#[test]
fn test_node_prop_value_top_level_fn_returns_both_null() {
    let tmp = indexed_nested();
    let path = tmp.path();
    for (file, name) in [("a.py", "top"), ("a.py", "outer"), ("a.ts", "tsTop")] {
        assert_eq!(
            owner_row(path, file, name),
            serde_json::json!([null, null]),
            "{file}:{name}"
        );
    }
}

/// Stands for: Dart, Kotlin and Swift scope no kind, so a nested fn keeps
/// its stored owner (none) and has no lexical parent: both fields stay null,
/// as `ownerClass` printed before the change.
#[test]
fn test_node_prop_value_unscoped_language_nested_fn_returns_both_null() {
    let tmp = indexed_nested();
    let path = tmp.path();
    for (file, name) in [
        ("c.dart", "dartInner"),
        ("c.kt", "ktInner"),
        ("c.swift", "swInner"),
    ] {
        assert_eq!(
            owner_row(path, file, name),
            serde_json::json!([null, null]),
            "{file}:{name}"
        );
    }
}

/// Stands for: a Python local class is no scoped node. Its stored owner is
/// the enclosing fn (`stamp_owner_fn_by_span`), so `ownerClass` stays
/// `maker`, and `lexicalParent` prints that stored owner. This changes when
/// FU-2026-10-05-ff801e095d23 part (c) stops the `stamp_owner_fn_by_span`
/// write (after design_uid Phase 2); update this test then.
#[test]
fn test_node_prop_value_python_local_class_returns_stamped_owner_until_part_c() {
    let tmp = indexed_nested();
    assert_eq!(
        owner_row(tmp.path(), "a.py", "Fake"),
        serde_json::json!(["maker", "maker"])
    );
}

/// Stands for: `WHERE n.ownerClass = 'outer'` no longer matches a closure;
/// `n.lexicalParent` does, in a WHERE clause, an inline property map (the
/// second property switch, `node_prop_no_cache`) and an `IN` list. An empty
/// or whitespace-only value matches no node.
#[test]
fn test_node_prop_no_cache_where_owner_class_outer_no_longer_matches_closure() {
    let tmp = indexed_nested();
    let path = tmp.path();
    // A single-column RETURN renders each row as the bare value, not a 1-array.
    let names = |query: &str| -> Vec<Value> { nested_rows(path, query) };
    let inner = vec![Value::from("inner")];
    assert_eq!(
        names("MATCH (n) WHERE n.ownerClass = 'outer' RETURN n.name"),
        Vec::<Value>::new()
    );
    assert_eq!(
        names("MATCH (n:Function {ownerClass: 'outer'}) RETURN n.name"),
        Vec::<Value>::new()
    );
    assert_eq!(
        names("MATCH (n) WHERE n.lexicalParent = 'outer' RETURN n.name"),
        inner
    );
    assert_eq!(
        names("MATCH (n:Function {lexicalParent: 'outer'}) RETURN n.name"),
        inner
    );
    assert_eq!(
        names(
            "MATCH (n) WHERE n.lexicalParent IN ['outer', 'K::m'] \
             RETURN n.name ORDER BY n.name"
        ),
        vec![Value::from("helper"), Value::from("inner")]
    );
    for blank in ["", " "] {
        assert_eq!(
            names(&format!(
                "MATCH (n) WHERE n.lexicalParent = '{blank}' RETURN n.name"
            )),
            Vec::<Value>::new(),
            "{blank:?}"
        );
    }
}

/// Stands for: the json columns name both properties as written, so a
/// parser reads `n.lexicalParent` beside `n.ownerClass`.
#[test]
fn test_node_prop_value_json_columns_name_owner_class_and_lexical_parent() {
    let tmp = indexed_nested();
    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (n) WHERE n.name = 'inner' RETURN n.ownerClass, n.lexicalParent",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        out["columns"],
        serde_json::json!(["n.ownerClass", "n.lexicalParent"]),
        "{out}"
    );
}
