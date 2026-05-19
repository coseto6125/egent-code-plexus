//! Verifies the `gnx hook <event> --claude-code` subcommand parses
//! and dispatches without panic on a minimal stdin envelope. Per-event
//! behaviour is exercised in dedicated test files (T2-T7).

use std::io::Write;
use std::process::{Command, Stdio};

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn pre_tool_use_no_match_returns_empty_stdout() {
    let mut child = Command::new(gnx_bin())
        .args(["hook", "pre-tool-use", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"cwd": "/tmp", "tool_name": "Bash", "tool_input": {"command": "ls"}}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "expected empty stdout for no-op, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn missing_host_flag_errors() {
    let out = Command::new(gnx_bin())
        .args(["hook", "pre-tool-use"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit when --claude-code is missing"
    );
}
