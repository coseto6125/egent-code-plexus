use std::io::Write;
use std::process::{Command, Stdio};

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn hook_handle_exits_on_non_committed_stage() {
    let mut child = Command::new(cgn_bin())
        .args(["hook-handle", "prepared"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn hook_handle_silently_ignores_non_delete_events() {
    let mut child = Command::new(cgn_bin())
        .args(["hook-handle", "committed"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"abc123 def456 refs/heads/main\n")
        .unwrap();
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success());
}
