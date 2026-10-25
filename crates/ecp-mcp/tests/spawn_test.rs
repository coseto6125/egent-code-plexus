//! Unit test for spawn-mode dispatch — invokes a stub script that
//! echoes its arguments back, then verifies dispatch wrapped it
//! correctly. Avoids depending on a built ecp binary for this layer.

mod common;

use common::{stub_guard, write_stub};
use ecp_mcp::schema::DerivedTool;
use ecp_mcp::spawn::run_spawn;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::TempDir;

fn dummy_tool(subcommand: &str) -> DerivedTool {
    DerivedTool {
        name: format!("ecp_{subcommand}"),
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
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"sub=$1 arg1=$2 arg2=$3\"\n");
    let tool = dummy_tool("inspect");
    let out = run_spawn(&stub, &tool, &json!({"name": "foo"})).unwrap();
    assert!(out.contains("sub=inspect"));
    assert!(out.contains("arg1=--name"));
    assert!(out.contains("arg2=foo"));
}

// Contract, not a regression of spawn.rs: every ecp graph command carries its
// caveat in the stdout payload (`result`), and `run_spawn` returns stdout only.
// The stub plants a payload with a `result` caveat on stdout and progress noise
// on stderr; the MCP client must read the caveat and none of the noise, so a
// machine-parsed payload stays intact. It guards the stdout-only rule of
// spawn.rs; the CLI tests in `ecp-cli/tests/caveat_payload.rs` check that each
// graph command puts the caveat there.
#[test]
#[cfg(unix)]
fn test_run_spawn_caveat_in_stdout_payload() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = write_stub(
        dir.path(),
        "#!/bin/sh\necho 'l2.warm-attach sibling=/x rebuild=background' >&2\n\
         printf '{\"status\":\"success\",\"result\":\"graph is behind HEAD\"}\\n'\n",
    );
    let tool = dummy_tool("routes");
    let out = run_spawn(&stub, &tool, &json!({})).unwrap();
    let payload: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(payload["result"], "graph is behind HEAD", "got: {out}");
    assert!(
        !out.contains("warm-attach"),
        "stderr leaked into stdout: {out}"
    );
}

#[test]
fn spawn_subprocess_failure_returns_err_with_stderr() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho 'boom' >&2\nexit 1\n");
    let tool = dummy_tool("inspect");
    let err = run_spawn(&stub, &tool, &json!({})).unwrap_err();
    assert!(err.to_string().contains("boom"));
}

/// Stands for: a refusal (an ambiguous `impact` target) prints its
/// re-runnable candidates and caveat on stdout before the non-zero exit, so
/// the error an MCP client gets must carry them, beside stderr.
#[test]
fn test_run_spawn_nonzero_exit_with_stdout_payload_err_carries_payload() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = write_stub(
        dir.path(),
        "#!/bin/sh\necho '{\"status\":\"ambiguous\",\"candidates\":[{\"target\":\"load --file K.java\"}],\"result\":\"could not pair\"}'\necho 'ambiguous target' >&2\nexit 2\n",
    );
    let tool = dummy_tool("impact");
    let err = run_spawn(&stub, &tool, &json!({})).unwrap_err().to_string();
    for part in [
        "ambiguous target",
        "candidates",
        "load --file K.java",
        "could not pair",
    ] {
        assert!(err.contains(part), "missing {part:?}: {err}");
    }
}

/// Stands for: a failure with nothing on stdout keeps the stderr-only error.
#[test]
fn test_run_spawn_nonzero_exit_empty_stdout_err_has_no_stdout_section() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho 'boom' >&2\nexit 1\n");
    let tool = dummy_tool("inspect");
    let err = run_spawn(&stub, &tool, &json!({})).unwrap_err().to_string();
    assert!(err.contains("boom") && !err.contains("stdout:"), "{err}");
}

#[test]
fn spawn_peels_subcmd_arg_as_first_prefix() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"sub=$1 a1=$2 a2=$3 a3=$4\"\n");
    let mut tool = dummy_tool("peers");
    tool.subcmd_arg = Some("subcmd".into());
    tool.schema = Arc::new(json!({
        "properties": {
            "subcmd": { "type": "string", "enum": ["status", "diff"] }
        }
    }));
    tool.positional_args = vec!["peer".into()];
    let out = run_spawn(&stub, &tool, &json!({"subcmd": "diff", "peer": "sess-x"})).unwrap();
    assert!(out.contains("sub=peers"), "got: {out}");
    assert!(out.contains("a1=diff"), "got: {out}");
    assert!(out.contains("a2=sess-x"), "got: {out}");
}

#[test]
fn spawn_rejects_invalid_subcmd_value() {
    let _guard = stub_guard();
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
    assert!(err.to_string().contains("must be one of"), "got: {err}");
}

#[test]
fn spawn_errors_when_subcmd_missing() {
    let _guard = stub_guard();
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

/// Stub that rejects `@<group>` exactly as the real ecp binary does after T14:
/// exits non-zero and emits the "ecp group find" hint to stderr.
fn write_group_rejecting_stub(dir: &std::path::Path) -> std::path::PathBuf {
    write_stub(
        dir,
        r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    @all) ;;
    @*) echo "error: cannot be used at the top level — use \`ecp group find\` instead" >&2; exit 1 ;;
  esac
done
echo ok
"#,
    )
}

#[test]
fn mcp_find_at_group_rejects_with_hint() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = write_group_rejecting_stub(dir.path());
    let tool = dummy_tool("find");
    let err = run_spawn(&stub, &tool, &json!({"repo": "@demo", "pattern": "x"})).unwrap_err();
    assert!(
        err.to_string().contains("ecp group find"),
        "expected hint in error; got: {err}"
    );
}

#[test]
fn mcp_find_at_all_still_works() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = write_group_rejecting_stub(dir.path());
    let tool = dummy_tool("find");
    let out = run_spawn(&stub, &tool, &json!({"repo": "@all", "pattern": "x"})).unwrap();
    assert_eq!(out.trim(), "ok");
}
