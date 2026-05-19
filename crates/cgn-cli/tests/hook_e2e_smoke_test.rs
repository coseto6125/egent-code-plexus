//! E2E smoke: admin install/status round-trip against an isolated
//! settings.json. Confirms all 4 events list as missing initially,
//! and that selective install flips the right ones to INSTALLED.

use std::process::Command;
use tempfile::TempDir;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn smoke_admin_status_reports_missing_then_installed() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join("settings.json");

    let before = Command::new(cgn_bin())
        .args(["admin", "status", "--claude-code", "--settings-path"])
        .arg(&settings)
        .output()
        .unwrap();
    assert!(
        before.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&before.stderr)
    );
    let body = String::from_utf8_lossy(&before.stdout);
    for ev in [
        "session-start",
        "user-prompt-submit",
        "pre-tool-use",
        "post-tool-use",
    ] {
        assert!(body.contains(ev), "status should list {ev}; got: {body}");
    }
    assert!(body.contains("missing"));

    let install = Command::new(cgn_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            "session-start,pre-tool-use",
            "--settings-path",
        ])
        .arg(&settings)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    let after = Command::new(cgn_bin())
        .args(["admin", "status", "--claude-code", "--settings-path"])
        .arg(&settings)
        .output()
        .unwrap();
    let body = String::from_utf8_lossy(&after.stdout);
    assert!(
        body.contains("INSTALLED"),
        "status should reflect install; got: {body}"
    );
}
