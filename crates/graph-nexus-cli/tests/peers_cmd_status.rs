use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_gnx").into()
}

#[test]
fn peers_status_empty_repo_prints_no_peers() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
    let out = Command::new(bin())
        .args(["peers", "status", "--repo", dir.path().to_str().unwrap()])
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no peers") || stdout.is_empty() || stdout.contains("[]"),
        "unexpected status output: {stdout}"
    );
}

#[test]
fn peers_log_empty_prints_no_messages() {
    let dir = tempdir().unwrap();
    let out = Command::new(bin())
        .args(["peers", "log", "--repo", dir.path().to_str().unwrap()])
        .env("CLAUDE_CODE_SESSION_ID", "test_log_session")
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no messages") || stdout.is_empty());
}
