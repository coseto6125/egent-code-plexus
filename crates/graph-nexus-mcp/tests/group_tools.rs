//! Smoke tests: `gnx_group` tool registration + spawn-argv shape for the
//! single tool fronting all 7 `gnx group <verb>` sub-subcommands via the
//! `subcmd` discriminator. Mirrors `peers_tools.rs`.

mod common;

use clap::{Args, CommandFactory, Parser, Subcommand};
use common::write_stub;
use graph_nexus_mcp::server::GnxMcpServer;
use graph_nexus_mcp::spawn::run_spawn;
use serde_json::json;
use tempfile::TempDir;

// ── minimal synthetic CLI tree mirroring `gnx group` being `hide = true` ────

#[derive(Parser)]
#[command(name = "gnx")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmds,
}

#[derive(Subcommand)]
enum Cmds {
    /// Visible surrogate.
    Inspect(InspectArgs),
    /// Multi-repo group operations
    #[command(hide = true)]
    Group(GroupArgs),
}

#[derive(Args)]
struct InspectArgs {
    #[arg(long)]
    name: Option<String>,
}

#[derive(Args)]
struct GroupArgs {
    #[command(subcommand)]
    cmd: GroupCmd,
}

#[derive(Subcommand)]
enum GroupCmd {
    Sync { name: String },
    Find { name: String, pattern: String },
}

// ── registration tests ───────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn single_gnx_group_tool_registered() {
    let server = GnxMcpServer::new(&Cli::command()).expect("init");
    let names: Vec<&str> = server
        .list_tools()
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        names.contains(&"gnx_group"),
        "missing gnx_group; got {names:?}"
    );
    // The `hide = true` group subcommand must not produce its own
    // derived tool (the manual injection is the only entry).
    let group_count = names.iter().filter(|n| n.starts_with("gnx_group")).count();
    assert_eq!(
        group_count, 1,
        "expected exactly one gnx_group* tool; got {names:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn gnx_group_advertises_all_subcmds() {
    let server = GnxMcpServer::new(&Cli::command()).expect("init");
    let tool = server
        .list_tools()
        .iter()
        .find(|t| t.name == "gnx_group")
        .expect("gnx_group tool")
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
    for sub in ["sync", "status", "contracts", "impact", "find", "coverage"] {
        assert!(allowed.iter().any(|s| s == sub), "subcmd `{sub}` missing");
    }
    // `search` was folded into `find` (post-PR-146 consolidation); guard
    // against re-introduction.
    assert!(
        !allowed.iter().any(|s| s == "search"),
        "subcmd `search` must not reappear; merge/limit/batch live on `find`"
    );
}

// ── spawn argv shape tests ───────────────────────────────────────────────────

fn group_tool() -> graph_nexus_mcp::schema::DerivedTool {
    graph_nexus_mcp::group::group_tools()
        .into_iter()
        .find(|t| t.name == "gnx_group")
        .expect("gnx_group tool")
}

fn echo_stub(dir: &std::path::Path) -> std::path::PathBuf {
    write_stub(dir, "#!/bin/sh\necho \"$@\"\n")
}

#[test]
fn sync_emits_group_sync_with_name_positional() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let out = run_spawn(&stub, &tool, &json!({"subcmd": "sync", "name": "demo"})).unwrap();
    assert!(out.contains("group sync"), "expected 'group sync': {out:?}");
    assert!(out.contains("demo"), "got: {out:?}");
    // name must follow `sync`
    let sync_pos = out.find("sync").unwrap();
    let name_pos = out.find("demo").unwrap();
    assert!(name_pos > sync_pos, "name must follow sync: {out:?}");
}

#[test]
fn find_emits_group_find_with_name_then_pattern() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({"subcmd": "find", "name": "demo", "pattern": "Foo"}),
    )
    .unwrap();
    assert!(out.contains("group find"), "got: {out:?}");
    let find_pos = out.find("find").unwrap();
    let name_pos = out.find("demo").unwrap();
    let pat_pos = out.find("Foo").unwrap();
    assert!(name_pos > find_pos, "name after find: {out:?}");
    assert!(pat_pos > name_pos, "pattern after name: {out:?}");
}

#[test]
fn find_with_merge_rrf_and_limit_emits_unified_topk_flags() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({
            "subcmd": "find",
            "name": "demo",
            "pattern": "auth_handler",
            "merge": "rrf",
            "limit": 10,
        }),
    )
    .unwrap();
    assert!(out.contains("group find"), "got: {out:?}");
    assert!(out.contains("--merge"), "got: {out:?}");
    assert!(out.contains(" rrf"), "got: {out:?}");
    assert!(out.contains("--limit"), "got: {out:?}");
    assert!(out.contains(" 10"), "got: {out:?}");
    // pattern is positional and must precede flags
    let pat_pos = out.find("auth_handler").expect("pattern in out");
    let limit_pos = out.find("--limit").unwrap();
    assert!(pat_pos < limit_pos, "pattern must precede flags: {out:?}");
}

#[test]
fn find_with_batch_emits_bare_flag_no_pattern_needed() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({"subcmd": "find", "name": "demo", "batch": true}),
    )
    .unwrap();
    assert!(out.contains("group find"), "got: {out:?}");
    assert!(out.contains("--batch"), "got: {out:?}");
}

#[test]
fn impact_emits_target_and_repo_flags() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({
            "subcmd": "impact",
            "name": "demo",
            "target": "auth_check",
            "repo": "svc-auth",
            "cross_depth": 1,
        }),
    )
    .unwrap();
    assert!(out.contains("group impact"), "got: {out:?}");
    assert!(out.contains("--target"), "got: {out:?}");
    assert!(out.contains("auth_check"), "got: {out:?}");
    assert!(out.contains("--repo"), "got: {out:?}");
    assert!(out.contains("svc-auth"), "got: {out:?}");
    assert!(out.contains("--cross-depth"), "got: {out:?}");
}

#[test]
fn contracts_emits_filters_as_kebab_flags() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({
            "subcmd": "contracts",
            "name": "demo",
            "type": "http",
            "unmatched": true,
        }),
    )
    .unwrap();
    assert!(out.contains("group contracts"), "got: {out:?}");
    assert!(out.contains("--type"), "got: {out:?}");
    assert!(out.contains(" http"), "got: {out:?}");
    // boolean flag emitted bare
    assert!(out.contains("--unmatched"), "got: {out:?}");
}

#[test]
fn coverage_emits_minimal_argv() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let out = run_spawn(
        &stub,
        &tool,
        &json!({"subcmd": "coverage", "name": "demo", "json": true}),
    )
    .unwrap();
    assert!(out.contains("group coverage"), "got: {out:?}");
    assert!(out.contains("demo"), "got: {out:?}");
    assert!(out.contains("--json"), "got: {out:?}");
}

#[test]
fn missing_subcmd_returns_err() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let err = run_spawn(&stub, &tool, &json!({"name": "demo"})).unwrap_err();
    assert!(err.to_string().contains("missing required `subcmd`"));
}

#[test]
fn invalid_subcmd_returns_err_without_spawning() {
    let dir = TempDir::new().unwrap();
    let stub = echo_stub(dir.path());
    let tool = group_tool();
    let err = run_spawn(&stub, &tool, &json!({"subcmd": "add", "name": "demo"})).unwrap_err();
    assert!(err.to_string().contains("must be one of"), "got: {err:?}");
}
