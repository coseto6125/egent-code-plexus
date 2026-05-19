use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_gnx").into()
}

#[test]
fn watch_foreground_exits_immediately_in_test_mode() {
    let dir = tempdir().unwrap();
    let out = Command::new(bin())
        .args([
            "watch",
            "--foreground",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("CGN_TEST_EXIT_AFTER_INIT", "1")
        .output()
        .expect("spawn cgn");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn watch_status_when_no_watcher_running_returns_not_running() {
    let dir = tempdir().unwrap();
    let out = Command::new(bin())
        .args(["watch", "--status", "--repo", dir.path().to_str().unwrap()])
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("not running") || stdout.contains("no watcher"),
        "unexpected status output: {stdout}"
    );
}
