//! E2E fixture tests for two `ecp inspect` extensions:
//!
//! 1. `contained_variants` — for `NodeKind::Enum`, surface the Defines→
//!    EnumVariant children so LLM consumers can list enum members without
//!    a follow-up cypher query. Pre-existing `collect_contained_members`
//!    only walked HasMethod/HasProperty; Enum bodies were silently empty.
//!
//! 2. `symbol.decorators` — pulls the same FunctionMeta.decorators that
//!    cypher's `m.decorators` whitelist (PR #352) returns, with `@` prefix
//!    stripped, so the JSON inspect view and the cypher property return
//!    identical shapes.

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

fn names(arr: &Value) -> Vec<String> {
    arr.as_array()
        .map(|xs| {
            xs.iter()
                .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn string_list(arr: &Value) -> Vec<String> {
    arr.as_array()
        .map(|xs| {
            xs.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ── contained_variants ────────────────────────────────────────────────────

#[test]
fn rust_enum_inspect_lists_variants() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/color.rs",
        "pub enum Color { Red, Green, Blue }\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Color", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));

    let variants = names(&out["contained_variants"]);
    assert!(
        variants.iter().any(|n| n == "Red"),
        "expected Red in contained_variants, got {variants:?}"
    );
    assert!(
        variants.iter().any(|n| n == "Green"),
        "expected Green in contained_variants, got {variants:?}"
    );
    assert!(
        variants.iter().any(|n| n == "Blue"),
        "expected Blue in contained_variants, got {variants:?}"
    );
}

#[test]
fn java_enum_inspect_lists_variants() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Day.java",
        "public enum Day { MONDAY, TUESDAY, WEDNESDAY }\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Day", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));

    let variants = names(&out["contained_variants"]);
    assert_eq!(variants.len(), 3, "expected 3 variants, got {variants:?}");
}

#[test]
fn non_enum_class_has_empty_variants() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Foo.java",
        "public class Foo { void bar() {} }\n",
    );
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "Foo", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));

    // contained_variants must be present (typed) but empty for non-Enum kinds.
    assert!(out["contained_variants"].is_array());
    let variants = names(&out["contained_variants"]);
    assert!(
        variants.is_empty(),
        "expected empty variants on Class, got {variants:?}"
    );
}

// ── symbol.decorators ─────────────────────────────────────────────────────

#[test]
fn python_decorator_appears_in_symbol_block() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/svc.py",
        "import functools\n\nclass Svc:\n    @staticmethod\n    def make():\n        return Svc()\n\n    @functools.cached_property\n    def cached(self):\n        return 1\n",
    );
    init_and_analyze(tmp.path());

    let make = run_json(
        tmp.path(),
        &["inspect", "--name", "make", "--format", "json"],
    );
    let decs = string_list(&make["symbol"]["decorators"]);
    assert!(
        decs.iter().any(|d| d == "staticmethod"),
        "expected 'staticmethod' in symbol.decorators, got {decs:?}"
    );

    let cached = run_json(
        tmp.path(),
        &["inspect", "--name", "cached", "--format", "json"],
    );
    let decs = string_list(&cached["symbol"]["decorators"]);
    assert!(
        decs.iter().any(|d| d == "functools.cached_property"),
        "expected 'functools.cached_property' in symbol.decorators, got {decs:?}"
    );
}

#[test]
fn undecorated_function_has_empty_decorators() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/plain.py", "def plain():\n    return 1\n");
    init_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "plain", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"));
    // `decorators` must always be present as an array (never null / missing),
    // so LLM consumers can iterate unconditionally.
    let decs = &out["symbol"]["decorators"];
    assert!(decs.is_array(), "decorators must be array, got {decs:?}");
    assert!(
        decs.as_array().unwrap().is_empty(),
        "expected empty decorators on undecorated function, got {decs:?}"
    );
}
