//! Regression: hook handlers must emit at most ONE JSON object on stdout.
//! Claude Code parses stdout as a single JSON; two newline-separated
//! `println!` payloads silently drop the second.
//!
//! Trigger: any handler where two signals can be produced in one invocation
//! (graph hits + peer drain in pre-tool-use; rules template + peer drain in
//! session-start; rebuild marker + peer drain in user-prompt-submit).

use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_gnx").into()
}

fn write_meta(session_dir: &std::path::Path, sid: &str) {
    let meta = format!(
        r#"{{"version":1,"session_id":"{sid}","pid":{pid},"started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1}}"#,
        pid = std::process::id()
    );
    std::fs::write(session_dir.join("meta.json"), meta).unwrap();
}

fn fire(event: &str, envelope: &str, sid: &str, repo_root: &std::path::Path) -> Vec<u8> {
    let mut child = Command::new(bin())
        .args(["hook", event, "--claude-code"])
        .env("CLAUDE_CODE_SESSION_ID", sid)
        .env("CGN_REPO_ROOT_OVERRIDE", repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap().stdout
}

fn assert_single_json(stdout: &[u8], event_label: &str) -> Value {
    assert!(
        !stdout.is_empty(),
        "{event_label}: expected non-empty stdout when peer drain has data"
    );
    let s = String::from_utf8_lossy(stdout);
    let json_lines = s
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .count();
    assert_eq!(
        json_lines, 1,
        "{event_label}: expected exactly one JSON object on stdout, got {json_lines}\nstdout:\n{s}"
    );
    serde_json::from_str(s.trim())
        .unwrap_or_else(|e| panic!("{event_label}: stdout not single-JSON-parseable: {e}\n{s}"))
}

fn seed_inbox(session_dir: &std::path::Path, sid: &str, body: &str) {
    std::fs::create_dir_all(session_dir).unwrap();
    write_meta(session_dir, sid);
    let entry = format!(
        r#"{{"type":"message","ts":"2026-05-18T00:00:00Z","msg_id":"m_x","from":"peer","to":null,"reply_to":null,"body":"{body}"}}"#
    );
    std::fs::write(session_dir.join("inbox.jsonl"), format!("{entry}\n")).unwrap();
}

#[test]
fn pre_tool_use_emits_single_json_when_peer_drain_has_data() {
    let tmp = tempdir().unwrap();
    let sid = "single_emit_pre";
    let session_dir = tmp.path().join("sessions").join(sid);
    seed_inbox(&session_dir, sid, "pre-tool-peer-msg");

    // Envelope without a Grep pattern → only the peer drain path fires.
    // The single-JSON invariant must hold for any combination of paths.
    let out = fire("pre-tool-use", "{}", sid, tmp.path());
    let json = assert_single_json(&out, "PreToolUse");
    let ctx = json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        ctx.contains("pre-tool-peer-msg"),
        "peer body missing from payload: {ctx}"
    );
}

#[test]
fn user_prompt_submit_emits_single_json_when_peer_drain_has_data() {
    let tmp = tempdir().unwrap();
    let sid = "single_emit_ups";
    let session_dir = tmp.path().join("sessions").join(sid);
    seed_inbox(&session_dir, sid, "ups-peer-msg");

    let out = fire("user-prompt-submit", "{}", sid, tmp.path());
    let json = assert_single_json(&out, "UserPromptSubmit");
    let ctx = json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        ctx.contains("ups-peer-msg"),
        "peer body missing from payload: {ctx}"
    );
}

/// True regression for B1: seed BOTH a `.rebuild-complete` marker AND a
/// peer inbox entry so the handler builds two sections in one fire.
/// With the pre-fix double-`println!` code, this would emit two JSON
/// objects on stdout and `assert_single_json` would trip on json_lines==2.
#[test]
fn user_prompt_submit_coalesces_rebuild_marker_and_peer_drain() {
    let tmp = tempdir().unwrap();
    let sid = "single_emit_ups_dual";
    let session_dir = tmp.path().join("sessions").join(sid);
    seed_inbox(&session_dir, sid, "ups-dual-peer");

    // gnx_state_dir() requires an absolute cwd with `<cwd>/.gnx/` present.
    let cwd = tempdir().unwrap();
    let state_dir = cwd.path().join(".gnx");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join(".rebuild-complete"), b"").unwrap();

    let envelope = format!(r#"{{"cwd":"{}"}}"#, cwd.path().display());
    let out = fire("user-prompt-submit", &envelope, sid, tmp.path());
    let json = assert_single_json(&out, "UserPromptSubmit(dual)");
    let ctx = json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        ctx.contains("rebuild complete"),
        "rebuild section missing: {ctx}"
    );
    assert!(ctx.contains("ups-dual-peer"), "peer section missing: {ctx}");
}

#[test]
fn session_start_drains_peer_inbox() {
    let tmp = tempdir().unwrap();
    let sid = "single_emit_ss";
    let session_dir = tmp.path().join("sessions").join(sid);
    seed_inbox(&session_dir, sid, "ss-peer-msg");

    // cwd must be non-empty (else session_start returns immediately).
    // Use a tempdir; it won't match the registry, so the rules-template
    // path is a no-op and only peer drain produces output.
    let cwd = tmp.path().display();
    let envelope = format!(r#"{{"cwd":"{cwd}"}}"#);
    let out = fire("session-start", &envelope, sid, tmp.path());
    let json = assert_single_json(&out, "SessionStart");
    let ctx = json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        ctx.contains("ss-peer-msg"),
        "SessionStart should drain peer inbox, payload: {ctx}"
    );

    // Inbox should be truncated post-drain.
    let after = std::fs::read_to_string(session_dir.join("inbox.jsonl")).unwrap_or_default();
    assert!(
        after.is_empty() || !after.contains("ss-peer-msg"),
        "inbox should be truncated after SessionStart drain. after: {after}"
    );
}
