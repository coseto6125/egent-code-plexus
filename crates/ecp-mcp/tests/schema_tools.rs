//! Smoke tests for the hand-rolled `ecp_schema` MCP tool (FU-020).
//!
//! Verifies single-tool registration, the four subcmd enum values, and
//! the argv shape produced for each subcmd dispatch. Mirrors
//! `group_tools.rs` / `peers_tools.rs`.

mod common;

use clap::{Args, CommandFactory, Parser, Subcommand};
use common::{stub_guard, write_stub};
use ecp_mcp::server::EcpMcpServer;
use ecp_mcp::spawn::run_spawn;
use serde_json::json;
use tempfile::TempDir;

#[derive(Parser)]
#[command(name = "ecp")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmds,
}

#[derive(Subcommand)]
enum Cmds {
    /// Visible surrogate (required so the synthetic root has at least one).
    Inspect(InspectArgs),
    /// Schema inventory (hidden in production; the manual MCP tool is
    /// the only path MCP clients have to reach the sub-subcommands).
    #[command(hide = true)]
    Schema(SchemaRootArgs),
}

#[derive(Args)]
struct InspectArgs {
    #[arg(long)]
    name: Option<String>,
}

#[derive(Args)]
struct SchemaRootArgs {
    #[command(subcommand)]
    cmd: SchemaCmd,
}

#[derive(Subcommand)]
enum SchemaCmd {
    Blindspots(FmtArgs),
    Reltypes(FmtArgs),
    #[command(name = "node-kinds")]
    NodeKinds(FmtArgs),
    #[command(name = "graph-version")]
    GraphVersion(FmtArgs),
}

#[derive(Args)]
struct FmtArgs {
    #[arg(long, default_value = "json")]
    format: String,
}

#[tokio::test(flavor = "current_thread")]
async fn single_ecp_schema_tool_registered() {
    let server = EcpMcpServer::new(&Cli::command()).expect("init");
    let names: Vec<&str> = server
        .list_tools()
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        names.contains(&"ecp_schema"),
        "missing ecp_schema; got {names:?}"
    );
    let schema_count = names.iter().filter(|n| n.starts_with("ecp_schema")).count();
    assert_eq!(
        schema_count, 1,
        "expected exactly one ecp_schema* tool; got {names:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ecp_schema_advertises_all_subcmds() {
    let server = EcpMcpServer::new(&Cli::command()).expect("init");
    let tool = server
        .list_tools()
        .iter()
        .find(|t| t.name == "ecp_schema")
        .expect("ecp_schema tool")
        .clone();
    assert_eq!(tool.subcmd_arg.as_deref(), Some("subcmd"));
    let allowed = tool
        .schema
        .get("properties")
        .and_then(|p| p.get("subcmd"))
        .and_then(|s| s.get("enum"))
        .and_then(|e| e.as_array())
        .expect("subcmd enum")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    for sub in ["blindspots", "reltypes", "node-kinds", "graph-version"] {
        assert!(allowed.iter().any(|s| s == sub), "subcmd `{sub}` missing");
    }
}

fn schema_tool() -> ecp_mcp::schema::DerivedTool {
    ecp_mcp::schema_mcp::schema_tools()
        .into_iter()
        .find(|t| t.name == "ecp_schema")
        .expect("ecp_schema tool")
}

fn echo_stub(dir: &std::path::Path) -> std::path::PathBuf {
    write_stub(dir, "#!/bin/sh\necho \"$@\"\n")
}

#[test]
fn blindspots_emits_schema_blindspots_with_format_flag() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = schema_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({"subcmd": "blindspots", "format": "text"}),
    )
    .unwrap();
    assert!(
        out.contains("schema blindspots"),
        "expected 'schema blindspots': {out:?}"
    );
    assert!(out.contains("--format"), "got: {out:?}");
    assert!(out.contains(" text"), "got: {out:?}");
}

#[test]
fn graph_version_emits_dashed_subcmd() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = schema_tool();
    let out = run_spawn(&stub, &tool, &json!({"subcmd": "graph-version"})).unwrap();
    assert!(
        out.contains("schema graph-version"),
        "expected 'schema graph-version': {out:?}"
    );
}

#[test]
fn missing_subcmd_returns_err() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = schema_tool();
    let err = run_spawn(&stub, &tool, &json!({})).unwrap_err();
    assert!(err.to_string().contains("missing required `subcmd`"));
}

#[test]
fn invalid_subcmd_returns_err_without_spawning() {
    let _guard = stub_guard();
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = schema_tool();
    let err = run_spawn(&stub, &tool, &json!({"subcmd": "nodes"})).unwrap_err();
    assert!(err.to_string().contains("must be one of"), "got: {err:?}");
}
