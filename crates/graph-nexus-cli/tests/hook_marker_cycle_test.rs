//! UserPromptSubmit hook: surface .rebuild-{complete,failed} markers
//! then unlink them.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_with_envelope(cwd: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(gnx_bin())
        .args(["hook", "user-prompt-submit", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let envelope = format!(r#"{{"cwd": "{}"}}"#, cwd.display());
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn complete_marker_surfaced_and_unlinked() {
    let tmp = TempDir::new().unwrap();
    let gnx_dir = tmp.path().join(".gitnexus-rs");
    std::fs::create_dir_all(&gnx_dir).unwrap();
    std::fs::write(gnx_dir.join(".rebuild-complete"), "").unwrap();
    std::fs::write(
        gnx_dir.join("meta.json"),
        r#"{"indexed_at":"2026-05-16T00:00:00Z","node_count":42,"worktree_path":"/x","remote_url":"","schema_version":1}"#,
    )
    .unwrap();

    let out = run_with_envelope(tmp.path());
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("rebuild complete"), "got: {body}");
    assert!(body.contains("42"), "should mention node count");
    assert!(
        !gnx_dir.join(".rebuild-complete").exists(),
        "marker should be unlinked"
    );
}

#[test]
fn failed_marker_takes_priority_over_complete() {
    let tmp = TempDir::new().unwrap();
    let gnx_dir = tmp.path().join(".gitnexus-rs");
    std::fs::create_dir_all(&gnx_dir).unwrap();
    std::fs::write(gnx_dir.join(".rebuild-complete"), "").unwrap();
    std::fs::write(gnx_dir.join(".rebuild-failed"), "").unwrap();
    std::fs::write(
        gnx_dir.join("last-rebuild.log"),
        "line1\nline2\nfatal error\n",
    )
    .unwrap();

    let out = run_with_envelope(tmp.path());
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("FAILED"));
    assert!(body.contains("fatal error"));
    assert!(!gnx_dir.join(".rebuild-failed").exists());
}

#[test]
fn no_markers_yields_silent_no_op() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gitnexus-rs")).unwrap();
    let out = run_with_envelope(tmp.path());
    assert!(out.stdout.is_empty());
}
