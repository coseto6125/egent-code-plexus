//! Smoke tests: peer tool registration + spawn-argv shape for the single
//! `cgn_peers` tool fronting all sub-subcommands via `subcmd` discriminator.

mod common;

use clap::{Args, CommandFactory, Parser, Subcommand};
use common::write_stub;
use cgn_mcp::server::CgnMcpServer;
use cgn_mcp::spawn::run_spawn;
use serde_json::json;
use tempfile::TempDir;

// ── minimal synthetic CLI tree (no cgn binary needed) ────────────────────────

#[derive(Parser)]
#[command(name = "cgn")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmds,
}

#[derive(Subcommand)]
enum Cmds {
    /// Visible surrogate.
    Inspect(InspectArgs),
    /// Multi-session peer collaboration (status / diff / log / gc + Ƀ messaging)
    Peers(PeersArgs),
}

#[derive(Args)]
struct InspectArgs {
    #[arg(long)]
    name: Option<String>,
}

#[derive(Args)]
struct PeersArgs {
    #[command(subcommand)]
    cmd: PeersCmd,
}

#[derive(Subcommand)]
enum PeersCmd {
    Status,
    Log,
    Say { body: String },
}

// ── registration tests ────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn single_cgn_peers_tool_registered() {
    let server = CgnMcpServer::new(&Cli::command()).expect("init");
    let names: Vec<&str> = server
        .list_tools()
        .iter()
        .map(|t| t.name.as_str())
        .collect();

    assert!(
        names.contains(&"cgn_peers"),
        "missing cgn_peers; got {names:?}"
    );
    for stale in ["cgn_peers_status", "cgn_peers_log", "cgn_peers_say"] {
        assert!(
            !names.contains(&stale),
            "split-form `{stale}` must not appear; got {names:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cgn_peers_advertises_subcmd_discriminator() {
    let server = CgnMcpServer::new(&Cli::command()).expect("init");
    let tool = server
        .list_tools()
        .iter()
        .find(|t| t.name == "cgn_peers")
        .expect("cgn_peers tool")
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
    for sub in ["status", "diff", "log", "say", "inbox", "thread"] {
        assert!(allowed.iter().any(|s| s == sub), "subcmd `{sub}` missing");
    }
    assert!(
        !allowed.iter().any(|s| s == "gc"),
        "`gc` is maintenance-only and must not appear in subcmd enum"
    );
}

// ── spawn argv shape tests ────────────────────────────────────────────────────

fn peers_tool() -> cgn_mcp::schema::DerivedTool {
    cgn_mcp::peers::peer_tools()
        .into_iter()
        .find(|t| t.name == "cgn_peers")
        .expect("cgn_peers tool")
}

#[test]
fn status_subcmd_yields_peers_status_argv() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"$@\"\n");
    let tool = peers_tool();
    let out = run_spawn(&stub, &tool, &json!({"subcmd": "status"})).unwrap();
    assert!(
        out.contains("peers status"),
        "expected 'peers status' in argv echo, got: {out:?}"
    );
}

#[test]
fn log_subcmd_passes_limit_flag() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"$@\"\n");
    let tool = peers_tool();
    let out = run_spawn(&stub, &tool, &json!({"subcmd": "log", "limit": 10})).unwrap();
    assert!(out.contains("peers log"), "got: {out:?}");
    assert!(out.contains("--limit"), "got: {out:?}");
    assert!(out.contains(" 10"), "got: {out:?}");
}

#[test]
fn say_subcmd_emits_body_as_positional() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"$@\"\n");
    let tool = peers_tool();
    let out = run_spawn(&stub, &tool, &json!({"subcmd": "say", "body": "hello"})).unwrap();
    assert!(out.contains("peers say"), "got: {out:?}");
    assert!(out.contains("hello"), "got: {out:?}");
    // body must come before any flags (positional ordering)
    let say_pos = out.find("say").unwrap();
    let hello_pos = out.find("hello").unwrap();
    assert!(hello_pos > say_pos, "body must follow `say`: {out:?}");
}

#[test]
fn diff_subcmd_emits_peer_then_optional_symbol() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"$@\"\n");
    let tool = peers_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({"subcmd": "diff", "peer": "sess-x", "symbol": "Foo"}),
    )
    .unwrap();
    assert!(out.contains("peers diff"), "got: {out:?}");
    let peer_pos = out.find("sess-x").expect("peer in out");
    let sym_pos = out.find("Foo").expect("symbol in out");
    assert!(peer_pos < sym_pos, "peer must precede symbol: {out:?}");
}

#[test]
fn thread_subcmd_emits_msg_id_positional() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"$@\"\n");
    let tool = peers_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({"subcmd": "thread", "msg_id": "abc123"}),
    )
    .unwrap();
    assert!(out.contains("peers thread"), "got: {out:?}");
    assert!(out.contains("abc123"), "got: {out:?}");
}

#[test]
fn missing_subcmd_returns_err() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"$@\"\n");
    let tool = peers_tool();
    let err = run_spawn(&stub, &tool, &json!({})).unwrap_err();
    assert!(err.to_string().contains("missing required `subcmd`"));
}

#[test]
fn invalid_subcmd_returns_err_without_spawning() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho \"$@\"\n");
    let tool = peers_tool();
    let err = run_spawn(&stub, &tool, &json!({"subcmd": "gc"})).unwrap_err();
    assert!(
        err.to_string().contains("must be one of"),
        "got: {err:?}"
    );
}
