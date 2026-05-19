//! Auto-watch spawn: session_start checks for <repo>/.gnx/auto-watch marker.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_gnx").into()
}

fn run_session_start(cwd: &std::path::Path) -> std::process::Output {
    let envelope = format!(r#"{{"cwd":"{}"}}"#, cwd.display());
    let mut child = Command::new(bin())
        .args(["hook", "session-start", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gnx");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait_with_output")
}

#[test]
fn no_marker_no_spawn_no_error() {
    let dir = tempdir().unwrap();
    let out = run_session_start(dir.path());
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn marker_present_session_start_still_returns_quickly() {
    let dir = tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join(".gnx")).unwrap();
    std::fs::write(repo.join(".gnx/auto-watch"), "").unwrap();

    let start = std::time::Instant::now();
    let out = run_session_start(repo);
    let elapsed = start.elapsed();

    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed.as_secs() < 3,
        "session_start blocked on watcher spawn (took {elapsed:?})"
    );
}
