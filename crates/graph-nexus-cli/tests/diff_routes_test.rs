//! Verify `gnx diff --section routes --baseline <ref>` returns
//! Route node changes between two refs.

use std::process::Command;
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

const V1_ROUTES: &str = r#"
import express from "express";
const app = express();
app.get('/api/users', (req, res) => res.json({}));
"#;

const V2_ROUTES: &str = r#"
import express from "express";
const app = express();
app.get('/api/users', (req, res) => res.json({}));
app.post('/api/posts', (req, res) => res.json({}));
"#;

#[test]
fn diff_routes_two_commit_added() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init failed");

    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/routes.ts"), V1_ROUTES).expect("write v1");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v1",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "commit v1: {}",
        String::from_utf8_lossy(&out.stderr)
    );

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

    std::fs::write(repo.join("src/routes.ts"), V2_ROUTES).expect("write v2");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v2",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "commit v2: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let output = Command::new(gnx_bin())
        .args([
            "diff",
            "--section",
            "routes",
            "--baseline",
            &baseline_sha,
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("run gnx diff routes");

    assert!(
        output.status.success(),
        "gnx diff routes failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout: {stdout}"));

    let added = parsed["sections"]["routes"]["added"]
        .as_array()
        .expect("sections.routes.added must be array");

    assert!(
        added
            .iter()
            .any(|r| r["path"].as_str() == Some("/api/posts")
                && r["method"]
                    .as_str()
                    .map(|m| m.eq_ignore_ascii_case("POST"))
                    .unwrap_or(false)),
        "expected added route POST /api/posts; got: {added:?}"
    );
}

#[test]
fn diff_routes_head_vs_head_empty() {
    let head_sha = {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };
    let output = Command::new(gnx_bin())
        .args([
            "diff",
            "--section",
            "routes",
            "--baseline",
            &head_sha,
            "--format",
            "json",
        ])
        .output()
        .expect("run gnx diff routes");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let routes = &parsed["sections"]["routes"];
    for key in ["added", "removed", "modified"] {
        let arr = routes[key]
            .as_array()
            .unwrap_or_else(|| panic!("missing {key}"));
        assert!(arr.is_empty(), "{key} should be empty: {arr:?}");
    }
}
