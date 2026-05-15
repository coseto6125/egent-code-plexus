//! SessionStart hook: template render + worktree detection.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn no_index_present_yields_empty_output() {
    let tmp = TempDir::new().unwrap();
    let mut child = Command::new(gnx_bin())
        .args(["hook", "session-start", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let envelope = format!(r#"{{"cwd": "{}"}}"#, tmp.path().display());
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "no .gitnexus-rs/ and not a worktree → no-op expected"
    );
}

#[test]
fn template_placeholders_get_rendered_when_meta_present() {
    let tmp = TempDir::new().unwrap();
    let gnx_dir = tmp.path().join(".gitnexus-rs");
    std::fs::create_dir_all(&gnx_dir).unwrap();
    std::fs::write(
        gnx_dir.join("meta.json"),
        r#"{"indexed_at":"2026-05-16T00:00:00Z","node_count":1234,"worktree_path":"/x","remote_url":"","schema_version":1}"#,
    )
    .unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("gnx-rules.md"),
        "stats: {{stats.nodes}} symbols",
    )
    .unwrap();

    let mut child = Command::new(gnx_bin())
        .args(["hook", "session-start", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let envelope = format!(r#"{{"cwd": "{}"}}"#, tmp.path().display());
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        body.contains("1234 symbols"),
        "rendered output should substitute {{{{stats.nodes}}}}: got {body}"
    );
    assert!(body.contains("SessionStart"));
}
