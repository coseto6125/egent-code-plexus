use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_ecp").into()
}

fn write_meta(session_dir: &std::path::Path, sid: &str) {
    let meta = format!(
        r#"{{"version":1,"session_id":"{sid}","pid":{pid},"started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1}}"#,
        pid = std::process::id()
    );
    std::fs::write(session_dir.join("meta.json"), meta).unwrap();
}

#[test]
fn pre_tool_use_emits_peer_section_when_inbox_has_entries() {
    let dir = tempdir().unwrap();
    let me = "test_drain_sess";
    let session_dir = dir.path().join("sessions").join(me);
    std::fs::create_dir_all(&session_dir).unwrap();
    write_meta(&session_dir, me);
    let entry = r#"{"type":"message","ts":"2026-05-17T00:00:00Z","msg_id":"m1","from":"alice","to":null,"reply_to":null,"body":"hello-from-alice"}"#;
    std::fs::write(session_dir.join("inbox.jsonl"), format!("{entry}\n")).unwrap();

    let mut child = Command::new(bin())
        .args(["hook", "pre-tool-use", "--claude-code"])
        .env("ECP_SESSION_ID", me)
        .env("CLAUDE_CODE_SESSION_ID", me)
        .env("ECP_REPO_ROOT_OVERRIDE", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{}").unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hello-from-alice"),
        "rendered payload should contain message body. stdout: {stdout}"
    );
}

#[test]
fn pre_tool_use_silent_when_inbox_empty() {
    let dir = tempdir().unwrap();
    let me = "test_drain_empty";
    let session_dir = dir.path().join("sessions").join(me);
    std::fs::create_dir_all(&session_dir).unwrap();
    write_meta(&session_dir, me);
    // No inbox file

    let mut child = Command::new(bin())
        .args(["hook", "pre-tool-use", "--claude-code"])
        .env("ECP_SESSION_ID", me)
        .env("CLAUDE_CODE_SESSION_ID", me)
        .env("ECP_REPO_ROOT_OVERRIDE", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{}").unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("[ecp peers]"),
        "should not emit peers section when inbox empty. stdout: {stdout}"
    );
}

#[test]
fn pre_tool_use_truncates_inbox_after_drain() {
    let dir = tempdir().unwrap();
    let me = "test_drain_truncate";
    let session_dir = dir.path().join("sessions").join(me);
    std::fs::create_dir_all(&session_dir).unwrap();
    write_meta(&session_dir, me);
    let entry = r#"{"type":"message","ts":"t","msg_id":"m_t","from":"alice","to":null,"reply_to":null,"body":"truncate-me"}"#;
    std::fs::write(session_dir.join("inbox.jsonl"), format!("{entry}\n")).unwrap();

    let mut child = Command::new(bin())
        .args(["hook", "pre-tool-use", "--claude-code"])
        .env("ECP_SESSION_ID", me)
        .env("CLAUDE_CODE_SESSION_ID", me)
        .env("ECP_REPO_ROOT_OVERRIDE", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{}").unwrap();
    let _ = child.wait_with_output();

    let after = std::fs::read_to_string(session_dir.join("inbox.jsonl")).unwrap_or_default();
    assert!(
        after.is_empty() || !after.contains("truncate-me"),
        "inbox should be truncated after drain. after: {after}"
    );
}

#[test]
fn user_prompt_submit_also_drains_inbox() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let dir = tempdir().unwrap();
    let me = "test_ups_sess";
    let session_dir = dir.path().join("sessions").join(me);
    std::fs::create_dir_all(&session_dir).unwrap();
    let meta = format!(
        r#"{{"version":1,"session_id":"{me}","pid":{pid},"started_at":"t","last_touched":"t","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1}}"#,
        pid = std::process::id()
    );
    std::fs::write(session_dir.join("meta.json"), meta).unwrap();
    let entry = r#"{"type":"message","ts":"t","msg_id":"m_u","from":"bob","to":null,"reply_to":null,"body":"prompt-time-peek"}"#;
    std::fs::write(session_dir.join("inbox.jsonl"), format!("{entry}\n")).unwrap();

    let mut child = Command::new(bin())
        .args(["hook", "user-prompt-submit", "--claude-code"])
        .env("ECP_SESSION_ID", me)
        .env("CLAUDE_CODE_SESSION_ID", me)
        .env("ECP_REPO_ROOT_OVERRIDE", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{}").unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("prompt-time-peek"),
        "user_prompt_submit should also drain inbox. stdout: {stdout}"
    );
}
