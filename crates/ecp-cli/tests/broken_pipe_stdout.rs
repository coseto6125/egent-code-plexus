//! A reader that closes stdout early (`ecp find x | head -1`) must end ecp
//! the Unix way, by SIGPIPE, never by a panic. The release profile sets
//! `panic = "abort"`, so the panic `print!` raises on EPIPE became a SIGABRT
//! and a 1 GB core dump under WSL: six such dumps were found, all panicking
//! at `std/src/io/stdio.rs` "failed printing to stdout".
#![cfg(unix)]

use std::os::fd::OwnedFd;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};

const SOURCE: &str = "function foo() { return bar(); }\nfunction bar() { return 1; }\n";

fn ecp(repo: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ecp"));
    cmd.current_dir(repo)
        .env("HOME", repo)
        .env("ECP_SKIP_BG_REBUILD", "1");
    cmd
}

fn indexed_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.ts"), SOURCE).unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);
    let out = ecp(repo)
        .args(["admin", "index", "--repo", "."])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    tmp
}

#[test]
fn test_ecp_stdout_closed_before_write_ends_by_sigpipe_without_panic() {
    let tmp = indexed_repo();
    // The read end is closed before ecp starts, so its first stdout write
    // gets EPIPE with no race against the reader.
    let (read_end, write_end): (OwnedFd, OwnedFd) = nix::unistd::pipe().unwrap();
    drop(read_end);
    let out = ecp(tmp.path())
        .args(["find", "foo", "--repo", "."])
        .stdout(Stdio::from(write_end))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "a closed stdout must not panic: {stderr}"
    );
    assert_eq!(
        out.status.signal(),
        Some(nix::sys::signal::Signal::SIGPIPE as i32),
        "status {:?}, stderr: {stderr}",
        out.status
    );
}
