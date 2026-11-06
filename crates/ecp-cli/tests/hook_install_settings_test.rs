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
    assert!(pre
        .iter()
        .any(|entry| entry["matcher"] == "Bash|Edit|Write"));
}

/// Contract: the PostToolUse entry matches the Grep tool too, so a Grep
/// call reaches the search delta; Bash, Edit and Write stay matched.
#[test]
fn test_install_post_tool_use_matcher_adds_grep_keeps_bash_edit_write() {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(&settings_path, "{}").unwrap();
    let out = Command::new(ecp_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            "post-tool-use",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let post = merged["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post.len(), 1);
    assert_eq!(post[0]["matcher"], "Grep|Bash|Edit|Write");
}

/// Status reads an `ev` entry whose matcher is `stale` as missing, and
/// `install-hook` rewrites it to the one entry with matcher `current`.
fn assert_stale_matcher_reinstalls(ev: &str, camel: &str, stale: &str, current: &str) {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    let stale = serde_json::json!({"hooks": {camel: [{
        "matcher": stale,
        "hooks": [{"type": "command",
                   "command": format!("\"/old/ecp\" hook {ev} --claude-code"),
                   "timeout": 5}],
    }]}});
    std::fs::write(&settings_path, stale.to_string()).unwrap();
    let status = || {
        let out = Command::new(ecp_bin())
            .args(["admin", "status", "--claude-code", "--settings-path"])
            .arg(&settings_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let line = |text: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(ev))
            .unwrap_or_else(|| panic!("no {ev} line: {text}"))
            .to_owned()
    };
    assert!(line(&status()).contains("missing"), "{}", status());

    let out = Command::new(ecp_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            ev,
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(line(&status()).contains("INSTALLED"), "{}", status());
    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let entries = merged["hooks"][camel].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{merged}");
    assert_eq!(entries[0]["matcher"], current);
}

/// Contract: settings written by the previous release keep the PostToolUse
/// matcher `Bash|Edit|Write`, which never reaches a Grep call.
#[test]
fn test_status_stale_post_tool_use_matcher_reads_missing_until_reinstall() {
    assert_stale_matcher_reinstalls(
        "post-tool-use",
        "PostToolUse",
        "Bash|Edit|Write",
        "Grep|Bash|Edit|Write",
    );
}

/// Contract: the PreToolUse entry no longer matches Grep or Glob. Its graph
/// hits moved to the PostToolUse search delta, so a search would spawn a
/// second hook process with nothing to say; a peer message drains at the
/// next Bash, Edit, Write or prompt instead.
#[test]
fn test_status_stale_pre_tool_use_matcher_reads_missing_until_reinstall() {
    assert_stale_matcher_reinstalls(
        "pre-tool-use",
        "PreToolUse",
        "Grep|Glob|Bash|Edit|Write",
        "Bash|Edit|Write",
    );
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

#[test]
fn uninstall_all_events_drops_hooks_key_entirely() {
    // After removing every event ecp installed, settings.json must NOT
    // retain a hollow `"hooks": {}` — that's the visible leftover this
    // test guards against. A user's pre-existing non-ecp fields stay.
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"theme":"dark","editor":{"font_size":14}}"#,
    )
    .unwrap();

    Command::new(ecp_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            "session-start,pre-tool-use,post-tool-use,user-prompt-submit",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();

    Command::new(ecp_bin())
        .args([
            "admin",
            "uninstall-hook",
            "--claude-code",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();

    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(
        merged.get("hooks").is_none(),
        "hooks key must be dropped entirely, got: {merged}"
    );
    // Non-ecp settings preserved.
    assert_eq!(merged["theme"], "dark");
    assert_eq!(merged["editor"]["font_size"], 14);
}

/// `agent-dispatch` installs as a SECOND PreToolUse entry with its own
/// `Agent|Task` matcher, coexisting with the enrichment hook's
/// `Grep|Glob|Bash` entry — and uninstalls independently of it.
#[test]
fn agent_dispatch_coexists_with_enrichment_and_uninstalls_alone() {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(&settings_path, "{}").unwrap();

    let install = Command::new(ecp_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            "pre-tool-use,agent-dispatch",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(install.status.success());

    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let pre = merged["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 2, "two independent PreToolUse entries");
    let tripwire = pre
        .iter()
        .find(|e| {
            e["hooks"][0]["command"]
                .as_str()
                .unwrap_or("")
                .contains("hook agent-dispatch")
        })
        .expect("agent-dispatch entry present");
    assert_eq!(tripwire["matcher"].as_str(), Some("Agent|Task"));

    let uninstall = Command::new(ecp_bin())
        .args([
            "admin",
            "uninstall-hook",
            "--claude-code",
            "--events",
            "agent-dispatch",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(uninstall.status.success());

    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let pre = merged["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 1, "tripwire removed, enrichment survives");
    assert!(pre[0]["hooks"][0]["command"]
        .as_str()
        .unwrap_or("")
        .contains("hook pre-tool-use"));
}
