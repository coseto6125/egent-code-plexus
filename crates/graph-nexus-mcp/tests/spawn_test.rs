//! Unit test for spawn-mode dispatch — invokes a stub script that
//! echoes its arguments back, then verifies dispatch wrapped it
//! correctly. Avoids depending on a built gnx binary for this layer.

use graph_nexus_mcp::schema::DerivedTool;
use graph_nexus_mcp::spawn::run_spawn;
use serde_json::json;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
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
        schema: Arc::new(json!({})),
        flag_args: HashSet::new(),
        positional_args: Vec::new(),
        prefix_args: Vec::new(),
        subcmd_arg: None,
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

#[test]
fn spawn_peels_subcmd_arg_as_first_prefix() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(
        dir.path(),
        "#!/bin/sh\necho \"sub=$1 a1=$2 a2=$3 a3=$4\"\n",
    );
    let mut tool = dummy_tool("peers");
    tool.subcmd_arg = Some("subcmd".into());
    tool.schema = Arc::new(json!({
        "properties": {
            "subcmd": { "type": "string", "enum": ["status", "diff"] }
        }
    }));
    tool.positional_args = vec!["peer".into()];
    let out = run_spawn(
        &stub,
        &tool,
        &json!({"subcmd": "diff", "peer": "sess-x"}),
    )
    .unwrap();
    assert!(out.contains("sub=peers"), "got: {out}");
    assert!(out.contains("a1=diff"), "got: {out}");
    assert!(out.contains("a2=sess-x"), "got: {out}");
}

#[test]
fn spawn_rejects_invalid_subcmd_value() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho ok\n");
    let mut tool = dummy_tool("peers");
    tool.subcmd_arg = Some("subcmd".into());
    tool.schema = Arc::new(json!({
        "properties": {
            "subcmd": { "type": "string", "enum": ["status"] }
        }
    }));
    let err = run_spawn(&stub, &tool, &json!({"subcmd": "evil"})).unwrap_err();
    assert!(
        err.to_string().contains("must be one of"),
        "got: {err}"
    );
}

#[test]
fn spawn_errors_when_subcmd_missing() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho ok\n");
    let mut tool = dummy_tool("peers");
    tool.subcmd_arg = Some("subcmd".into());
    let err = run_spawn(&stub, &tool, &json!({})).unwrap_err();
    assert!(
        err.to_string().contains("missing required `subcmd`"),
        "got: {err}"
    );
}
