//! PreToolUse hook: pattern extraction + in-process graph augmentation.
//! The "with index → emit hits" branch is exercised in T8 (needs a
//! built fixture); here we pin pattern extraction + no-op branches.

use std::io::Write;
use std::process::{Command, Stdio};

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run(envelope: &str) -> std::process::Output {
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
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn short_pattern_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"ab"}}"#);
    assert!(out.stdout.is_empty(), "<3 char pattern should no-op");
}

#[test]
fn missing_graph_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"validateUser"}}"#);
    assert!(out.stdout.is_empty(), "no .gitnexus-rs/ in /tmp → no-op");
}

#[test]
fn bash_grep_no_index_no_op() {
    let out = run(
        r#"{"cwd":"/tmp","tool_name":"Bash","tool_input":{"command":"rg -n 'validateUser' src/"}}"#,
    );
    assert!(
        out.stdout.is_empty(),
        "no index → no-op even with valid pattern"
    );
    assert!(out.status.success(), "hook must never fail on no-op");
}

#[test]
fn non_search_tool_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Read","tool_input":{"file_path":"foo"}}"#);
    assert!(out.stdout.is_empty());
}

#[test]
fn glob_pattern_with_no_index_no_op() {
    let out = run(
        r#"{"cwd":"/tmp","tool_name":"Glob","tool_input":{"pattern":"src/**/validateUser.rs"}}"#,
    );
    assert!(out.stdout.is_empty());
}
