//! settings.json merge for install / uninstall.

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn install_one_event_creates_entry_preserves_others() {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    let initial = r#"{
  "hooks": {
    "UserPromptSubmit": [
      {"matcher":"","hooks":[{"type":"command","command":"node /legacy/gitnexus-hook.cjs","timeout":3}]}
    ]
  }
}"#;
    std::fs::write(&settings_path, initial).unwrap();

    let out = Command::new(ecp_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            "session-start",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();

    let user_prompt = merged["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert!(
        user_prompt.iter().any(|e| {
            e["hooks"][0]["command"]
                .as_str()
                .unwrap_or("")
                .contains("legacy/gitnexus-hook.cjs")
        }),
        "legacy entry preserved"
    );

    let session_start = merged["hooks"]["SessionStart"].as_array().unwrap();
    assert!(
        session_start.iter().any(|e| {
            e["hooks"][0]["command"]
                .as_str()
                .unwrap_or("")
                .contains("hook session-start --claude-code")
        }),
        "new entry written"
    );
}

#[test]
fn reinstalling_same_event_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(&settings_path, "{}").unwrap();

    for _ in 0..2 {
        let out = Command::new(ecp_bin())
            .args([
                "admin",
                "install-hook",
                "--claude-code",
                "--events",
                "pre-tool-use",
                "--settings-path",
            ])
            .arg(&settings_path)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let pre = merged["hooks"]["PreToolUse"].as_array().unwrap();
    let count = pre
        .iter()
        .filter(|e| {
            e["hooks"][0]["command"]
                .as_str()
                .unwrap_or("")
                .contains("hook pre-tool-use")
        })
        .count();
    assert_eq!(count, 1, "duplicate entries should not accumulate");
}

#[test]
fn uninstall_removes_only_specified_event() {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(&settings_path, "{}").unwrap();

    let install = Command::new(ecp_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            "session-start,pre-tool-use",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(install.status.success());

    let uninstall = Command::new(ecp_bin())
        .args([
            "admin",
            "uninstall-hook",
            "--claude-code",
            "--events",
            "session-start",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(uninstall.status.success());

    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let session = merged["hooks"]
        .get("SessionStart")
        .map(|v| v.as_array().map(|a| a.is_empty()).unwrap_or(true))
        .unwrap_or(true);
    assert!(session, "SessionStart removed");
    assert!(
        merged["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| {
                e["hooks"][0]["command"]
                    .as_str()
                    .unwrap_or("")
                    .contains("hook pre-tool-use")
            }),
        "PreToolUse retained"
    );
}
