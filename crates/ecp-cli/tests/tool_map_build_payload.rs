//! Smoke test for `build_payload` extraction in `commands::tool_map`.
//!
//! Uses the binary integration pattern (Option A): invokes `ecp tool-map`
//! via the compiled binary and asserts the JSON shape is intact —
//! `{status, totals, calls}` — confirming `build_payload` returns the
//! same value that `run` previously emitted to `emit()`.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

mod common;
use common::{ecp_bin, run_git};

fn init_repo_with_ts(repo: &Path) {
    let src = r#"
import axios from "axios";

export async function fetchUser(id: string) {
    const r = await axios.get(`/api/users/${id}`);
    return r;
}
"#;
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/main.ts"), src).unwrap();

    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(repo, &["add", "-A"]);
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );

    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index spawn failed");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn tool_map_build_payload_returns_status_totals_calls() {
    let repo = tempfile::tempdir().unwrap();
    init_repo_with_ts(repo.path());

    let out = Command::new(ecp_bin())
        .args(["tool-map", "--repo", ".", "--format", "json"])
        .current_dir(repo.path())
        .env("HOME", repo.path())
        .output()
        .expect("ecp tool-map failed to spawn");

    assert!(
        out.status.success(),
        "ecp tool-map failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "expected JSON on stdout, got:\nstdout={stdout}\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let val: Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"));

    assert_eq!(val["status"], "success", "expected status=success: {val}");
    assert!(val.get("totals").is_some(), "missing `totals` field: {val}");
    assert!(val.get("calls").is_some(), "missing `calls` field: {val}");
}
