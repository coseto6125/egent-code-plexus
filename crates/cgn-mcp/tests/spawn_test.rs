//! Unit test for spawn-mode dispatch — invokes a stub script that
//! echoes its arguments back, then verifies dispatch wrapped it
//! correctly. Avoids depending on a built cgn binary for this layer.

mod common;

use common::write_stub;
use cgn_mcp::schema::DerivedTool;
use cgn_mcp::spawn::run_spawn;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::TempDir;

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

// ─── @<group> rejection (T15) ────────────────────────────────────────────────

/// Stub that rejects `@<group>` exactly as the real cgn binary does after T14:
/// exits non-zero and emits the "cgn group find" hint to stderr.
fn write_group_rejecting_stub(dir: &std::path::Path) -> std::path::PathBuf {
    write_stub(
        dir,
        r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    @all) ;;
    @*) echo "error: cannot be used at the top level — use \`cgn group find\` instead" >&2; exit 1 ;;
  esac
done
echo ok
"#,
    )
}

#[test]
fn mcp_find_at_group_rejects_with_hint() {
    let dir = TempDir::new().unwrap();
    let stub = write_group_rejecting_stub(dir.path());
    let tool = dummy_tool("find");
    let err = run_spawn(&stub, &tool, &json!({"repo": "@demo", "pattern": "x"})).unwrap_err();
    assert!(
        err.to_string().contains("cgn group find"),
        "expected hint in error; got: {err}"
    );
}

#[test]
fn mcp_find_at_all_still_works() {
    let dir = TempDir::new().unwrap();
    let stub = write_group_rejecting_stub(dir.path());
    let tool = dummy_tool("find");
    let out = run_spawn(&stub, &tool, &json!({"repo": "@all", "pattern": "x"})).unwrap();
    assert_eq!(out.trim(), "ok");
}
