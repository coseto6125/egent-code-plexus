//! PostToolUse hook: git mutation → stale check → background reindex.
//! The "stale → spawn" branch is exercised end-to-end in T8 (needs a
//! real git+index fixture); here we pin the no-op branches.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run_with(envelope: &str) -> std::process::Output {
    let mut child = Command::new(ecp_bin())
        .args(["hook", "post-tool-use", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn non_bash_tool_no_op() {
    let out = run_with(r#"{"tool_name":"Read","tool_input":{"file_path":"x"}}"#);
    assert!(out.stdout.is_empty());
}

#[test]
fn non_git_bash_command_no_op() {
    let out = run_with(
        r#"{"tool_name":"Bash","tool_input":{"command":"ls -la"},"tool_output":{"exit_code":0}}"#,
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn failed_git_commit_no_op() {
    let out = run_with(
        r#"{"tool_name":"Bash","tool_input":{"command":"git commit -m foo"},"tool_output":{"exit_code":1}}"#,
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn git_commit_in_dir_without_index_no_op() {
    let tmp = TempDir::new().unwrap();
    let envelope = format!(
        r#"{{"cwd":"{}","tool_name":"Bash","tool_input":{{"command":"git commit -m foo"}},"tool_output":{{"exit_code":0}}}}"#,
        tmp.path().display()
    );
    let out = run_with(&envelope);
    assert!(out.stdout.is_empty(), "no registry entry → no-op");
}

#[test]
fn quoted_git_inside_echo_does_not_trigger() {
    // strip_shell_quotes should strip "git commit" inside the quoted
    // string before the regex test runs.
    let out = run_with(
        r#"{"tool_name":"Bash","tool_input":{"command":"echo \"git commit -m foo\""},"tool_output":{"exit_code":0}}"#,
    );
    assert!(
        out.stdout.is_empty(),
        "quoted git inside echo should not trigger"
    );
}
