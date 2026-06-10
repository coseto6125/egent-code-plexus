//! TDD: bare `ecp admin status` (no --claude-code) must report all hosts
//! rather than erroring with "invalid argument: --claude-code required".

use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Bare `ecp admin status` must exit 0 and mention all three hosts.
#[test]
fn bare_admin_status_reports_all_hosts() {
    let tmp = tempfile::tempdir().unwrap();

    let out = Command::new(ecp_bin())
        .args(["admin", "status"])
        .env("HOME", tmp.path())
        .output()
        .expect("ecp admin status failed to spawn");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "bare `admin status` must exit 0; stderr={stderr}\nstdout={stdout}"
    );
    // All three host headers must appear.
    assert!(
        stdout.contains("Claude Code") || stdout.to_lowercase().contains("claude"),
        "output must mention Claude Code; stdout={stdout}"
    );
    assert!(
        stdout.contains("Codex") || stdout.to_lowercase().contains("codex"),
        "output must mention Codex; stdout={stdout}"
    );
    assert!(
        stdout.contains("Gemini") || stdout.to_lowercase().contains("gemini"),
        "output must mention Gemini; stdout={stdout}"
    );
}

/// `--claude-code` narrows to Claude Code only (back-compat).
#[test]
fn admin_status_claude_code_flag_still_works() {
    let tmp = tempfile::tempdir().unwrap();

    let out = Command::new(ecp_bin())
        .args(["admin", "status", "--claude-code"])
        .env("HOME", tmp.path())
        .output()
        .expect("ecp admin status --claude-code failed to spawn");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "`admin status --claude-code` must exit 0; stderr={stderr}\nstdout={stdout}"
    );
    assert!(
        stdout.contains("Claude Code") || stdout.to_lowercase().contains("claude"),
        "narrowed output must mention Claude Code; stdout={stdout}"
    );
}
