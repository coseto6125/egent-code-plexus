//! Integration tests for Rust module-qualified call resolution.
//!
//! Pre-fix behaviour: `resolve_qualifier_file` only treated the immediate
//! qualifier as a Type lookup, so calls like `auto_ensure::ensure_fresh(...)`
//! never produced incoming edges — `auto_ensure` is a *module* (a `mod foo;`
//! declaration corresponding to `foo.rs` in the same crate), not a Type. On
//! the actual `gitnexus-rs` repo this left ≥40% of internal `fn`s with zero
//! reported callers, masking the real blast radius for any LLM driving
//! `cgn inspect`.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
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

fn run_json(repo: &Path, args: &[&str]) -> Value {
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
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|err| panic!("{args:?} did not return JSON: {err}\nstdout={stdout}"))
}

#[test]
fn rust_intra_crate_module_qualified_call_resolves_to_incoming() {
    // `crate_caller::caller_fn` invokes `auto_ensure::ensure_fresh()`. The
    // qualifier `auto_ensure` is a module declared by `mod auto_ensure;` and
    // backed by `src/auto_ensure.rs` — no Type with that name exists. After
    // the Tier-4 module-file fallback, `cgn inspect ensure_fresh` must list
    // `caller_fn` under incoming.calls.
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
        "src/auto_ensure.rs",
        "pub fn ensure_fresh() -> i32 { 1 }\n",
    );
    write(
        repo,
        "src/lib.rs",
        r#"mod auto_ensure;

pub fn caller_fn() -> i32 {
    auto_ensure::ensure_fresh()
}
"#,
    );
    init_and_analyze(repo);

    let result = run_json(
        repo,
        &["inspect", "--name", "ensure_fresh", "--format", "json"],
    );
    assert_eq!(result["status"], "found", "{result}");
    let incoming_calls = result["incoming"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let caller_names: Vec<&str> = incoming_calls
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        caller_names.iter().any(|n| *n == "caller_fn"),
        "expected `caller_fn` in incoming.calls; got names={caller_names:?}\nfull={result}"
    );
}

#[test]
fn rust_std_qualified_call_does_not_falsely_resolve_to_workspace_file() {
    // The workspace happens to have `src/fs.rs` with a `pub fn read()`.
    // A caller's `std::fs::read(...)` must NOT bind to `fs.rs::read` —
    // std::fs is external, the FQN crate-root prefix mismatches the caller's.
    // (`split_qualifier` strips to `("fs", "read")`; the Tier-4 fallback's
    // same-crate-prefix guard is what stops the false edge.)
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
    write(repo, "src/fs.rs", "pub fn read() -> i32 { 1 }\n");
    write(
        repo,
        "src/lib.rs",
        r#"pub fn caller_fn() -> std::io::Result<Vec<u8>> {
    std::fs::read("/tmp/x")
}
"#,
    );
    init_and_analyze(repo);

    let result = run_json(repo, &["inspect", "--name", "read", "--format", "json"]);
    if result["status"] != "found" {
        // Ambiguous if multiple `read` symbols exist; check each match block.
        let matches = result["matches"].as_array().cloned().unwrap_or_default();
        for m in matches {
            let fp = m["symbol"]["filePath"].as_str().unwrap_or("");
            if !fp.ends_with("src/fs.rs") {
                continue;
            }
            let callers: Vec<&str> = m["incoming"]["calls"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e["name"].as_str())
                .collect();
            assert!(
                !callers.iter().any(|n| *n == "caller_fn"),
                "workspace fs.rs::read must not bind to std::fs::read caller; callers={callers:?}"
            );
        }
        return;
    }
    let fp = result["symbol"]["filePath"].as_str().unwrap_or("");
    if !fp.ends_with("src/fs.rs") {
        return;
    }
    let callers: Vec<&str> = result["incoming"]["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        !callers.iter().any(|n| *n == "caller_fn"),
        "workspace fs.rs::read must not bind to std::fs::read caller; callers={callers:?}"
    );
}

#[test]
fn rust_module_qualified_disambiguates_when_two_crates_share_module_stem() {
    // Two workspace members each define their own `auto_ensure.rs`. A call
    // from `crates/a/src/lib.rs` using `auto_ensure::ping` must bind to
    // `crates/a/src/auto_ensure.rs`, not the sibling crate's version. This
    // is the same-crate-prefix guard in action when the Tier-4 lookup
    // happens via the stem→file index (multiple files share the stem; only
    // the in-crate one survives the prefix filter).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "Cargo.toml",
        r#"[workspace]
members = ["crates/a", "crates/b"]
resolver = "2"
"#,
    );
    write(
        repo,
        "crates/a/Cargo.toml",
        r#"[package]
name = "a"
version = "0.1.0"
edition = "2021"
"#,
    );
    write(
        repo,
        "crates/a/src/auto_ensure.rs",
        "pub fn ping() -> i32 { 1 }\n",
    );
    write(
        repo,
        "crates/a/src/lib.rs",
        r#"mod auto_ensure;

pub fn caller_a() -> i32 {
    auto_ensure::ping()
}
"#,
    );
    write(
        repo,
        "crates/b/Cargo.toml",
        r#"[package]
name = "b"
version = "0.1.0"
edition = "2021"
"#,
    );
    write(
        repo,
        "crates/b/src/auto_ensure.rs",
        "pub fn ping() -> i32 { 2 }\n",
    );
    write(repo, "crates/b/src/lib.rs", "pub fn unused_b() {}\n");
    init_and_analyze(repo);

    let result = run_json(repo, &["inspect", "--name", "ping", "--format", "json"]);
    let blocks: Vec<serde_json::Value> = if result["status"] == "found" {
        vec![result.clone()]
    } else {
        result["matches"].as_array().cloned().unwrap_or_default()
    };

    let mut crate_a_callers: Vec<String> = Vec::new();
    let mut crate_b_callers: Vec<String> = Vec::new();
    for block in &blocks {
        let symbol_file = block
            .get("symbol")
            .and_then(|s| s["filePath"].as_str())
            .unwrap_or("");
        let callers = block
            .get("incoming")
            .and_then(|i| i["calls"].as_array())
            .cloned()
            .unwrap_or_default();
        for entry in &callers {
            let name = entry["name"].as_str().unwrap_or("").to_string();
            if symbol_file.contains("crates/a/") {
                crate_a_callers.push(name);
            } else if symbol_file.contains("crates/b/") {
                crate_b_callers.push(name);
            }
        }
    }

    assert!(
        crate_a_callers.iter().any(|n| n == "caller_a"),
        "crate a's ping must list caller_a; crate_a_callers={crate_a_callers:?}"
    );
    assert!(
        !crate_b_callers.iter().any(|n| n == "caller_a"),
        "crate b's ping must NOT list caller_a; crate_b_callers={crate_b_callers:?}"
    );
}
