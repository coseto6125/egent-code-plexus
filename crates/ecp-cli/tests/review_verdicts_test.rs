//! Verify `ecp review --verdicts --format json` returns verdicts with correct
//! severity levels based on caller relationships.

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// v1: two functions, bar calls foo
const V1_CODE: &str = r#"
function foo() {
  return 42;
}

function bar() {
  return foo();
}
"#;

/// v2: foo's body changed (return 99 instead of 42)
const V2_CODE: &str = r#"
function foo() {
  return 99;
}

function bar() {
  return foo();
}
"#;

#[test]
fn review_verdicts_intra_caller_marks_warn() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init failed");

    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/functions.ts"), V1_CODE).expect("write v1");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git add v1");

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

    std::fs::write(repo.join("src/functions.ts"), V2_CODE).expect("write v2");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git add v2");

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
        .env("HOME", repo)
        .env("ECP_NO_PROGRESS", "1")
        .output()
        .expect("run ecp review verdicts");

    assert!(
        output.status.success(),
        "ecp review verdicts failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout: {stdout}"));

    // ──────────────────────────────────────────────────────────────────────
    // Check top-level keys exist
    // ──────────────────────────────────────────────────────────────────────
    assert!(
        parsed.get("baseline").is_some(),
        "missing top-level key: baseline"
    );
    assert!(
        parsed.get("current").is_some(),
        "missing top-level key: current"
    );
    assert!(
        parsed.get("verdicts").is_some(),
        "missing top-level key: verdicts"
    );
    assert!(
        parsed.get("summary").is_some(),
        "missing top-level key: summary"
    );
    assert!(
        parsed.get("elapsed_ms").is_some(),
        "missing top-level key: elapsed_ms"
    );

    // ──────────────────────────────────────────────────────────────────────
    // baseline and current have ref and sha
    // ──────────────────────────────────────────────────────────────────────
    assert!(
        parsed["baseline"].get("ref").is_some(),
        "baseline missing ref"
    );
    assert!(
        parsed["baseline"].get("sha").is_some(),
        "baseline missing sha"
    );
    assert!(
        parsed["current"].get("ref").is_some(),
        "current missing ref"
    );
    assert!(
        parsed["current"].get("sha").is_some(),
        "current missing sha"
    );

    let verdicts = parsed["verdicts"]
        .as_array()
        .expect("verdicts must be array");
    assert!(!verdicts.is_empty(), "verdicts array should not be empty");

    // ──────────────────────────────────────────────────────────────────────
    // Find the SIGNATURE_OR_BODY_CHANGED verdict for foo
    // ──────────────────────────────────────────────────────────────────────
    let foo_verdict = verdicts
        .iter()
        .find(|v| {
            v["kind"].as_str() == Some("SIGNATURE_OR_BODY_CHANGED")
                && v["symbol"].as_str() == Some("foo")
        })
        .unwrap_or_else(|| {
            panic!("expected a SIGNATURE_OR_BODY_CHANGED verdict for foo; got: {verdicts:?}")
        });

    // ──────────────────────────────────────────────────────────────────────
    // Check severity is WARN (not INFO, not RISK) due to intra-file callers
    // ──────────────────────────────────────────────────────────────────────
    assert_eq!(
        foo_verdict["severity"].as_str(),
        Some("WARN"),
        "expected severity WARN for foo (has intra-file callers); got: {}",
        foo_verdict["severity"]
    );

    // ──────────────────────────────────────────────────────────────────────
    // Check intra_callers is non-null with bar as a caller
    // ──────────────────────────────────────────────────────────────────────
    let intra_callers = foo_verdict["intra_callers"]
        .as_array()
        .expect("intra_callers must be non-null array for foo");
    assert!(
        !intra_callers.is_empty(),
        "intra_callers should not be empty"
    );

    let bar_found = intra_callers
        .iter()
        .any(|caller| caller["name"].as_str() == Some("bar"));
    assert!(
        bar_found,
        "expected bar in intra_callers; got: {intra_callers:?}"
    );

    // ──────────────────────────────────────────────────────────────────────
    // Verify summary has the expected counts
    // ──────────────────────────────────────────────────────────────────────
    let summary = &parsed["summary"];
    assert!(summary.get("total").is_some(), "summary missing total");
    assert!(summary.get("risk").is_some(), "summary missing risk");
    assert!(summary.get("warn").is_some(), "summary missing warn");
    assert!(summary.get("info").is_some(), "summary missing info");

    let warn_count = summary["warn"]
        .as_u64()
        .expect("summary.warn must be number");
    assert!(
        warn_count > 0,
        "expected at least 1 WARN verdict in summary; got warn={warn_count}"
    );
}
