//! Integration tests for `ecp impact --batch`.
//!
//! Tests cover:
//!   - 2+ targets happy path: divider + per-target JSON blocks
//!   - Mixed found/not-found: each target emits its own block; missing symbols
//!     get a per-target error entry, never abort the batch
//!   - --batch + --baseline is an invalid-argument error (symbol-mode only)
//!   - Flags like --direction/--depth apply uniformly to all targets
//!   - Blank lines and `#` comments are skipped
//!   - Empty stdin emits a stderr hint and zero output blocks

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::ecp_bin;

const SOURCE_CORE: &str = r#"
export class Base {
    baseMethod(): number { return 1; }
}

export class Greeter extends Base {
    greet(): string { return "hi"; }
}

export function helper(): number {
    return 1;
}

export function caller(): number {
    const g = new Greeter();
    return helper();
}
"#;

fn init_repo_and_analyze(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.ts"), SOURCE_CORE).unwrap();

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

fn run_batch_with_stdin(
    repo: &Path,
    stdin_payload: &str,
    extra_args: &[&str],
) -> std::process::Output {
    let mut args = vec!["impact", "--batch", "--repo", ".", "--format", "json"];
    args.extend_from_slice(extra_args);
    let mut child = Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

// ── Happy-path: 2 known symbols ───────────────────────────────────────────────

#[test]
fn impact_batch_emits_per_target_divider_lines() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_batch_with_stdin(tmp.path(), "caller\nhelper\n", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "--batch with 2 valid targets must succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("=== target: caller ==="),
        "missing divider for 'caller' in:\n{stdout}"
    );
    assert!(
        stdout.contains("=== target: helper ==="),
        "missing divider for 'helper' in:\n{stdout}"
    );
}

#[test]
fn impact_batch_each_block_contains_json_with_target_field() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_batch_with_stdin(tmp.path(), "caller\nhelper\n", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());

    // Split on dividers and parse each JSON block independently.
    let blocks: Vec<&str> = stdout
        .split("=== target: ")
        .filter(|b| !b.trim().is_empty())
        .collect();
    assert_eq!(blocks.len(), 2, "expected 2 target blocks:\n{stdout}");

    for block in &blocks {
        // First line of each block is "<name> ===" — rest is JSON.
        let json_start = block.find('{').expect("block must contain a JSON object");
        let json: serde_json::Value = serde_json::from_str(&block[json_start..])
            .unwrap_or_else(|e| panic!("invalid JSON in block: {e}\nblock={block}"));
        // Must carry status + target fields just like single-target mode.
        assert!(
            json.get("status").is_some() || json.get("error").is_some(),
            "block JSON missing status/error: {json}"
        );
        assert!(
            json.get("target").is_some() || json.get("error").is_some(),
            "block JSON missing target field: {json}"
        );
    }
}

#[test]
fn impact_batch_direction_applies_to_all_targets() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // caller --calls--> helper, so downstream from `caller` reaches `helper`;
    // upstream from `caller` reaches nothing (it's a leaf caller).
    // With --direction down, both block JSONs must reflect "downstream".
    let out = run_batch_with_stdin(tmp.path(), "caller\nhelper\n", &["--direction", "down"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());

    let json_count = stdout.matches("\"direction\"").count();
    assert!(
        json_count >= 2,
        "--direction must appear in every per-target JSON block:\n{stdout}"
    );
    let downstream_count = stdout.matches("\"downstream\"").count();
    assert!(
        downstream_count >= 2,
        "--direction down must set direction=downstream in every block:\n{stdout}"
    );
}

// ── Mixed found / not-found ───────────────────────────────────────────────────

#[test]
fn impact_batch_not_found_target_gets_error_entry_not_abort() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // "caller" is real; "nonexistentSymbol" is not.
    let out = run_batch_with_stdin(tmp.path(), "caller\nnonexistentSymbol\n", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Process must succeed despite one missing symbol.
    assert!(
        out.status.success(),
        "--batch must not abort on a not-found target; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Both dividers must appear.
    assert!(
        stdout.contains("=== target: caller ==="),
        "caller divider missing:\n{stdout}"
    );
    assert!(
        stdout.contains("=== target: nonexistentSymbol ==="),
        "not-found divider missing:\n{stdout}"
    );
    // The not-found block must carry an error/status field.
    let nef_block_start = stdout.find("=== target: nonexistentSymbol ===").unwrap();
    let nef_block = &stdout[nef_block_start..];
    let json_start = nef_block
        .find('{')
        .expect("not-found block must contain JSON");
    let json: serde_json::Value =
        serde_json::from_str(&nef_block[json_start..]).unwrap_or_else(|e| {
            let snippet = &nef_block[json_start..json_start.saturating_add(500)];
            panic!("invalid JSON for not-found block: {e}\nsnippet={snippet}")
        });
    assert!(
        json.get("error").is_some()
            || json["status"].as_str() == Some("not_found")
            || json["status"].as_str() == Some("error"),
        "not-found block must carry error or status=not_found: {json}"
    );
}

// ── --batch + --baseline is invalid ──────────────────────────────────────────

#[test]
fn impact_batch_with_baseline_is_invalid_argument() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(ecp_bin())
        .args(["impact", "--batch", "--baseline", "HEAD~1", "--repo", "."])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
        .wait_with_output()
        .unwrap();

    assert!(
        !out.status.success(),
        "--batch + --baseline must be rejected as invalid"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--batch")
            && (stderr.contains("baseline")
                || stderr.contains("invalid")
                || stderr.contains("cannot")
                || stderr.contains("conflict")),
        "expected --batch/--baseline conflict error:\n{stderr}"
    );
}

// ── Edge cases ────────────────────────────────────────────────────────────────

#[test]
fn impact_batch_skips_blank_and_commented_lines() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let payload = "\n# comment\n   \ncaller\n# trailing\n";
    let out = run_batch_with_stdin(tmp.path(), payload, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());

    let divider_count = stdout.matches("=== target: ").count();
    assert_eq!(
        divider_count, 1,
        "expected exactly 1 divider (only 'caller' is a real query): {divider_count}\n{stdout}"
    );
}

#[test]
fn impact_batch_empty_stdin_emits_no_blocks_and_stderr_hint() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_batch_with_stdin(tmp.path(), "\n# only comments\n", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "empty stdin must succeed: stderr={stderr}"
    );
    assert!(
        !stdout.contains("=== target: "),
        "empty stdin must produce zero blocks:\n{stdout}"
    );
    assert!(
        stderr.contains("batch") && stderr.contains("stdin"),
        "expected hint about missing stdin targets:\n{stderr}"
    );
}

/// A positional name combined with --batch would be silently discarded
/// (stdin is the single source of targets) — clap must reject it instead.
#[test]
fn impact_batch_rejects_positional_name() {
    let out = Command::new(ecp_bin())
        .args(["impact", "someSymbol", "--batch"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "positional name + --batch must be a clap error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected clap conflict error, got: {stderr}"
    );
}
