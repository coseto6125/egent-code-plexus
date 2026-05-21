//! Integration test for Rust inline-module qualified call resolution.
//!
//! Rust has two flavours of `mod foo`:
//!
//!   1. **File-backed** — `mod foo;` declared in a parent file, with a
//!      sibling `foo.rs` (or `foo/mod.rs`) holding the items. Qualified
//!      calls like `foo::bar()` resolve through the language-specific
//!      Tier 3.5 (module-tree FQN) or Tier 4 (module-file fallback)
//!      paths in the resolver — those walk filesystem layout.
//!
//!   2. **Inline** — `mod foo { ... }` declared with an inline body.
//!      No backing file. Tier 4 module-file fallback can't fire (no
//!      `foo.rs`). Before `is_qualifier` accepted `NodeKind::Module`,
//!      Tier 1 same-file qualifier lookup also rejected it (only Type
//!      kinds were accepted), so the call dropped entirely.
//!
//! This test pins the inline-mod fix: a single-file crate with
//! `mod foo { pub fn bar() {} }` and a `caller()` that invokes
//! `foo::bar()` must list `caller` in `bar`'s `incoming.calls`.

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
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|err| panic!("{args:?} did not return JSON: {err}\nstdout={stdout}"))
}

fn incoming_caller_names(result: &Value) -> Vec<String> {
    result["incoming"]["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["name"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn rust_inline_mod_qualified_call_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "Cargo.toml",
        r#"[package]
name = "demo"
version = "0.1.0"
edition = "2021"
"#,
    );
    write(
        repo,
        "src/lib.rs",
        r#"mod foo {
    pub fn bar() -> i32 { 42 }
}

pub fn caller() -> i32 {
    foo::bar()
}
"#,
    );
    init_and_analyze(repo);

    let result = run_json(repo, &["inspect", "--name", "bar", "--format", "json"]);
    assert_eq!(result["status"], "found", "{result}");
    let callers = incoming_caller_names(&result);
    assert!(
        callers.iter().any(|n| n == "caller"),
        "inline-mod `foo::bar()` must list `caller`; got {callers:?}\nfull={result}"
    );
}
