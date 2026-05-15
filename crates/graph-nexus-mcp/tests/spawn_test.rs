//! Unit test for spawn-mode dispatch — invokes a stub script that
//! echoes its arguments back, then verifies dispatch wrapped it
//! correctly. Avoids depending on a built gnx binary for this layer.

use graph_nexus_mcp::schema::DerivedTool;
use graph_nexus_mcp::spawn::run_spawn;
use serde_json::json;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn write_stub(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    let stub = dir.join("gnx");
    std::fs::write(&stub, script).unwrap();
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();
    stub
}

fn dummy_tool(subcommand: &str) -> DerivedTool {
    DerivedTool {
        name: format!("gnx_{subcommand}"),
        subcommand: subcommand.into(),
        description: String::new(),
        schema: json!({}),
        flag_args: HashSet::new(),
        positional_args: Vec::new(),
    }
}

#[test]
fn spawn_invokes_subcommand_and_captures_stdout() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"sub=$1 arg1=$2 arg2=$3\"\n");
    let tool = dummy_tool("inspect");
    let out = run_spawn(&stub, &tool, &json!({"name": "foo"})).unwrap();
    assert!(out.contains("sub=inspect"));
    assert!(out.contains("arg1=--name"));
    assert!(out.contains("arg2=foo"));
}

#[test]
fn spawn_subprocess_failure_returns_err_with_stderr() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho 'boom' >&2\nexit 1\n");
    let tool = dummy_tool("inspect");
    let err = run_spawn(&stub, &tool, &json!({})).unwrap_err();
    assert!(err.to_string().contains("boom"));
}
