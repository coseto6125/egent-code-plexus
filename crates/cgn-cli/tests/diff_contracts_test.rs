//! Verify `cgn diff --section contracts --baseline <ref>` returns
//! contract changes between two refs.

use std::process::Command;
use tempfile::TempDir;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

/// v1: empty placeholder — no routes yet.
const V1_EMPTY: &str = r#"
// nothing yet
export {};
"#;

/// v2: two Express routes added → contracts extractor emits route-kind entries.
/// (bare fetch() calls are NOT emitted as contracts; only Route nodes and
/// Fetches edges from the cross-repo analyzer are. Express routes are the
/// reliable trigger for kind="route" contract entries.)
const V2_ROUTES: &str = r#"
import express from "express";
const app = express();
app.get('/api/users', (req, res) => res.json({}));
app.post('/api/posts', (req, res) => res.json({}));
"#;

#[test]
fn diff_contracts_two_commit_added_fetch() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init failed");

    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/server.ts"), V1_EMPTY).expect("write v1");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "v1"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "commit v1: {}", String::from_utf8_lossy(&out.stderr));

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

    std::fs::write(repo.join("src/server.ts"), V2_ROUTES).expect("write v2");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "v2"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "commit v2: {}", String::from_utf8_lossy(&out.stderr));

    let output = Command::new(cgn_bin())
        .args([
            "diff", "--section", "contracts",
            "--baseline", &baseline_sha,
            "--format", "json",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("run cgn diff contracts");

    assert!(
        output.status.success(),
        "cgn diff contracts failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout: {stdout}"));

    let added = parsed["sections"]["contracts"]["added"]
        .as_array()
        .expect("sections.contracts.added must be array");

    assert!(
        !added.is_empty(),
        "expected at least 1 added contract (kind=route) for new Express routes; got empty. full output: {parsed:?}"
    );
}

#[test]
fn diff_contracts_head_vs_head_empty() {
    let head_sha = {
        let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };
    let output = Command::new(cgn_bin())
        .args(["diff", "--section", "contracts", "--baseline", &head_sha, "--format", "json"])
        .output()
        .expect("run cgn diff contracts");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let contracts = &parsed["sections"]["contracts"];
    for key in ["added", "removed", "modified"] {
        assert!(
            contracts[key].as_array().expect("array").is_empty(),
            "{key} should be empty: {contracts:?}"
        );
    }
}
