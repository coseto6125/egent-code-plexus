//! Smoke: build a server from a synthetic clap tree, list tools.
//!
//! The fixture mirrors the gnx CLI's surface in miniature — two visible
//! subcommands, one hidden — so we can assert visibility filtering
//! without linking the full CLI binary.

use clap::{Args, CommandFactory, Parser, Subcommand};
use cgn_mcp::server::GnxMcpServer;

#[derive(Parser)]
#[command(name = "gnx")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmds,
}

#[derive(Subcommand)]
enum Cmds {
    /// Visible inspect surrogate.
    Inspect(InspectArgs),
    /// Visible search surrogate.
    Search(SearchArgs),
    /// Hidden subcommand — must NOT appear in the tools list.
    #[command(hide = true)]
    HookHandle,
}

#[derive(Args)]
struct InspectArgs {
    #[arg(long)]
    name: Option<String>,
}

#[derive(Args)]
struct SearchArgs {
    pattern: String,
}

#[tokio::test(flavor = "current_thread")]
async fn list_tools_filters_hidden_subcommands() {
    let server = GnxMcpServer::new(&Cli::command()).expect("init");
    let names: Vec<&str> = server
        .list_tools()
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert!(names.contains(&"gnx_inspect"));
    assert!(names.contains(&"gnx_search"));
    assert!(
        !names.iter().any(|n| n.contains("hook")),
        "hidden subcommand leaked into tool list: {names:?}"
    );
}
