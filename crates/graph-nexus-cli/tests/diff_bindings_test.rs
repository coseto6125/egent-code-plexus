//! Verify `gnx diff --section bindings --baseline <ref>` returns
//! resolver decision changes between two refs.

use std::process::Command;
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

/// v1: a.ts imports from ./b (resolves) and from ./c (unresolved — c.ts absent).
const V1_A_TS: &str = r#"
import { helper } from "./b";
import { other } from "./c";
export function main() { helper(); other(); }
"#;

const V1_B_TS: &str = r#"
export function helper() { return 1; }
"#;

/// v2: add c.ts so the previously-unresolved import resolves (Unresolved → ImportScoped).
const V2_C_TS: &str = r#"
export function other() { return 2; }
"#;

#[test]
fn diff_bindings_two_commit_resolution_change() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init failed");

    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/a.ts"), V1_A_TS).expect("write v1 a");
    std::fs::write(repo.join("src/b.ts"), V1_B_TS).expect("write v1 b");

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

    // v2: add c.ts so the unresolved import of `other` from "./c" resolves.
    std::fs::write(repo.join("src/c.ts"), V2_C_TS).expect("write c");

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

    let output = Command::new(gnx_bin())
        .args(["diff", "--section", "bindings", "--baseline", &baseline_sha, "--format", "json"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("run gnx diff bindings");

    assert!(
        output.status.success(),
        "gnx diff bindings failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout: {stdout}"));

    let bindings = &parsed["sections"]["bindings"];
    let total_changes =
        bindings["new_resolutions"].as_array().map(|a| a.len()).unwrap_or(0)
            + bindings["tier_changes"].as_array().map(|a| a.len()).unwrap_or(0)
            + bindings["target_changes"].as_array().map(|a| a.len()).unwrap_or(0)
            + bindings["removed"].as_array().map(|a| a.len()).unwrap_or(0);

    assert!(
        total_changes > 0,
        "expected at least one binding change across the two commits; got: {bindings:?}"
    );
}

#[test]
fn diff_bindings_against_head_yields_empty() {
    // Diff HEAD vs HEAD: no resolver decisions changed.
    let head_sha = {
        let out = Command::new("git").args(["rev-parse", "HEAD"]).output().unwrap().stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };
    let output = Command::new(gnx_bin())
        .args(["diff", "--section", "bindings", "--baseline", &head_sha,
               "--format", "json"])
        .output()
        .expect("run gnx diff bindings");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout was: {stdout}"));
    let bindings = &parsed["sections"]["bindings"];
    for key in ["new_resolutions", "tier_changes", "target_changes", "removed"] {
        let arr = bindings[key].as_array()
            .unwrap_or_else(|| panic!("missing {key}"));
        assert!(arr.is_empty(), "{key} should be empty for HEAD vs HEAD; got {arr:?}");
    }
}
