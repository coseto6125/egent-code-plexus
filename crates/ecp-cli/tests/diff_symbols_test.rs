//! Verify `ecp diff --section symbols --baseline <ref>` returns
//! symbol-level changes (added, removed, changed) with intra-file caller tracking.

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// v1: two functions, bar calls foo
const V1_SYMBOLS: &str = r#"
function foo() {
  return 42;
}

function bar() {
  return foo();
}
"#;

/// v2: foo's body changed, baz added
const V2_SYMBOLS: &str = r#"
function foo() {
  return 99;
}

function bar() {
  return foo();
}

function baz() {
  return 10;
}
"#;

#[test]
fn diff_symbols_added_changed_with_intra_callers() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init failed");

    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/functions.ts"), V1_SYMBOLS).expect("write v1");

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

    std::fs::write(repo.join("src/functions.ts"), V2_SYMBOLS).expect("write v2");

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
            "diff",
            "--section",
            "symbols",
            "--baseline",
            &baseline_sha,
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .env("ECP_NO_PROGRESS", "1")
        .output()
        .expect("run ecp diff symbols");

    assert!(
        output.status.success(),
        "ecp diff symbols failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout: {stdout}"));

    let certain = parsed["sections"]["symbols"]["certain"]
        .as_object()
        .expect("sections.symbols.certain must be object");

    // ──────────────────────────────────────────────────────────────────────
    // Check symbols_added contains baz
    // ──────────────────────────────────────────────────────────────────────
    let symbols_added = certain["symbols_added"]
        .as_array()
        .expect("symbols_added must be array");
    assert!(
        symbols_added
            .iter()
            .any(|sym| sym["name"].as_str() == Some("baz")),
        "expected baz in symbols_added; got: {symbols_added:?}"
    );

    // ──────────────────────────────────────────────────────────────────────
    // Check symbols_changed contains foo (body changed)
    // ──────────────────────────────────────────────────────────────────────
    let symbols_changed = certain["symbols_changed"]
        .as_array()
        .expect("symbols_changed must be array");
    assert!(
        symbols_changed
            .iter()
            .any(|sym| sym["name"].as_str() == Some("foo")),
        "expected foo in symbols_changed; got: {symbols_changed:?}"
    );

    // ──────────────────────────────────────────────────────────────────────
    // Check intra_file_callers contains foo with bar as a caller
    // ──────────────────────────────────────────────────────────────────────
    let intra_file_callers = certain["intra_file_callers"]
        .as_array()
        .expect("intra_file_callers must be array");
    let foo_caller_entry = intra_file_callers
        .iter()
        .find(|entry| entry["target_name"].as_str() == Some("foo"))
        .expect("expected an intra_file_callers entry for target foo");

    let callers = foo_caller_entry["callers"]
        .as_array()
        .expect("callers must be array");
    assert!(
        callers
            .iter()
            .any(|caller| caller["name"].as_str() == Some("bar")),
        "expected bar in callers of foo; got: {callers:?}"
    );
}
