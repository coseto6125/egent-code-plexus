//! Verify `indirect_dispatch_in_diff_region` verdict fires when a function
//! in the diff region contains an indirect dispatch (Rust `&dyn Trait`).
//!
//! Coverage: confirms the P0 wiring (CallMeta FLAG_DYNAMIC_DISPATCH →
//! `IndirectDispatchRef` → verdict) works on Rust. Other 5 supported langs
//! (C, C++, JS, TS, Python) share the `call_metas` path so this single
//! Rust fixture is sufficient for the wiring smoke test; per-lang dispatch
//! detection has its own unit coverage in `indirect_dispatch.rs`.

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// v1: caller receives a concrete `Foo` — direct dispatch, no FLAG_DYNAMIC_DISPATCH.
const V1_LIB_RS: &str = r#"
pub trait Handler {
    fn handle(&self);
}

pub struct Foo;
impl Handler for Foo {
    fn handle(&self) {}
}

pub fn run(h: Foo) {
    h.handle();
}
"#;

/// v2: caller receives `&dyn Handler` — FLAG_DYNAMIC_DISPATCH on `.handle()`.
const V2_LIB_RS: &str = r#"
pub trait Handler {
    fn handle(&self);
}

pub struct Foo;
impl Handler for Foo {
    fn handle(&self) {}
}

pub fn run(h: &dyn Handler) {
    h.handle();
}
"#;

const CARGO_TOML: &str = r#"[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#;

fn git(repo: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(repo: &std::path::Path, msg: &str) {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

#[test]
fn indirect_dispatch_verdict_fires_on_rust_dyn_trait_in_diff() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    // FU-2026-05-23-047: keep $HOME (→ ~/.ecp/) OUTSIDE the worktree so the
    // background tantivy writer spawned by build_l2 cannot race with the
    // `git stash push -u` that GitGuard runs inside `ecp review --verdicts`.
    let ecp_home = TempDir::new().expect("ecp_home tempdir");

    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), CARGO_TOML).unwrap();
    std::fs::write(repo.join("src/lib.rs"), V1_LIB_RS).unwrap();
    commit(repo, "v1 — direct dispatch");

    let baseline_sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    std::fs::write(repo.join("src/lib.rs"), V2_LIB_RS).unwrap();
    commit(repo, "v2 — switch run() to &dyn Handler");

    let output = Command::new(ecp_bin())
        .args([
            "review",
            "--since",
            &baseline_sha,
            "--verdicts",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", ecp_home.path())
        .env("ECP_NO_PROGRESS", "1")
        .output()
        .expect("run ecp review --verdicts");

    assert!(
        output.status.success(),
        "review verdicts failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout: {stdout}"));

    let verdicts = parsed["verdicts"]
        .as_array()
        .expect("verdicts must be array");

    // The fixture is tiny so there should be exactly one indirect dispatch
    // verdict (on `run` calling `.handle()` via &dyn Handler).
    let indir_verdict = verdicts.iter().find(|v| {
        v["kind"].as_str() == Some("INDIRECT_DISPATCH_IN_DIFF_REGION")
            && v["symbol"].as_str() == Some("run")
    });
    assert!(
        indir_verdict.is_some(),
        "expected INDIRECT_DISPATCH_IN_DIFF_REGION verdict for `run` after switch to &dyn Handler; got: {verdicts:#?}"
    );
    let v = indir_verdict.unwrap();
    assert_eq!(
        v["severity"].as_str(),
        Some("WARN"),
        "indirect dispatch verdict should be WARN (target candidates exist in graph); got: {}",
        v["severity"]
    );
}
