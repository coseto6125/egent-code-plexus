use std::io::Write;
use std::process::{Command, Stdio};

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn hook_handle_exits_on_non_committed_stage() {
    let mut child = Command::new(ecp_bin())
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
    let mut child = Command::new(ecp_bin())
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

/// Hooks that older installs left still run `ecp hook-handle` on every ref
/// update. git writes the whole ref list to the hook's stdin, so a handler
/// that exits before reading it gives git a broken pipe; 1 MiB is far past
/// any pipe buffer.
#[test]
fn test_run_stdin_larger_than_pipe_buffer_exits_zero() {
    let line = b"0000000000000000000000000000000000000000 1111111111111111111111111111111111111111 refs/heads/some-branch\n";
    let payload: Vec<u8> = line.iter().copied().cycle().take(1 << 20).collect();
    for stage in ["prepared", "committed", "aborted"] {
        let mut child = Command::new(ecp_bin())
            .args(["hook-handle", stage])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let written = child.stdin.take().unwrap().write_all(&payload);
        let status = child.wait().unwrap();
        assert!(
            written.is_ok(),
            "[{stage}] hook-handle stopped reading stdin: {written:?}"
        );
        assert!(status.success(), "[{stage}] exit status {status}");
    }
}
