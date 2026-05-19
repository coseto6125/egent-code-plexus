//! Concurrency invariant 4.5 — hook spawn flock serialises.
//!
//! Two concurrent `cgn` hook invocations must converge to exactly ONE
//! reindex side-effect (the second flock acquirer no-ops cleanly).
//! Mirrors the production shell template at
//! `crates/graph-nexus-cli/src/background.rs:73-91` (markerless branch).

use cgn_cli::flock_preamble;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn slow_noop_path() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent().unwrap()
                .parent().unwrap()
                .join("target")
        });
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let path = target_dir.join(profile).join("examples").join("slow_noop");
    if !path.exists() {
        // `cargo test` doesn't auto-build examples — invoke cargo directly so
        // a clean checkout works without manual `cargo build --example` setup.
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "graph-nexus", "--example", "slow_noop"])
            .status()
            .expect("spawn cargo build --example slow_noop");
        assert!(status.success(), "cargo build --example slow_noop failed");
    }
    path
}

/// Wraps `inner` with the production flock preamble so the test pins to
/// the same quoting + redirect behaviour as `spawn_bg` (not a hand-rolled copy).
fn flock_shell(lock: &Path, inner: &str) -> String {
    format!("{}{inner}\n", flock_preamble(lock))
}

#[test]
fn hook_concurrent_spawn_flock_serializes() {
    let bin = slow_noop_path();

    let tmp = tempfile::TempDir::new().unwrap();
    let lock = tmp.path().join("reindex.lock");
    let marker = tmp.path().join("marker.txt");
    let inner = format!("'{}' '{}'", bin.display(), marker.display());
    let shell = flock_shell(&lock, &inner);

    let mut handles = Vec::new();
    for _ in 0..2 {
        let shell = shell.clone();
        handles.push(std::thread::spawn(move || {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(&shell)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn shell");
            child.wait().expect("wait shell")
        }));
    }

    let statuses: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    for (i, s) in statuses.iter().enumerate() {
        assert!(s.success(), "shell wrapper #{i} exited non-zero: {s:?}");
    }

    let content = std::fs::read_to_string(&marker).unwrap_or_default();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly 1 reindex side-effect, got {}: {:?}",
        lines.len(),
        lines,
    );

    assert!(lock.exists(), "lock file not created");
}

#[test]
fn hook_serial_spawn_runs_each_time() {
    let bin = slow_noop_path();

    let tmp = tempfile::TempDir::new().unwrap();
    let lock = tmp.path().join("reindex.lock");
    let marker = tmp.path().join("marker.txt");
    let inner = format!("'{}' '{}'", bin.display(), marker.display());
    let shell = flock_shell(&lock, &inner);

    for _ in 0..2 {
        let status = Command::new("sh")
            .arg("-c")
            .arg(&shell)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("status");
        assert!(status.success());
    }

    let content = std::fs::read_to_string(&marker).unwrap_or_default();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "serial calls should each run; got {lines:?}");
}
