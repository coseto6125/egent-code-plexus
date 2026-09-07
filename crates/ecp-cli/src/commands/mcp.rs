//! `ecp mcp` subcommand: serve | tools.
//!
//! Tools are derived from the ecp CLI's `clap::Command` tree (see
//! `ecp-mcp::schema`). Every visible non-hidden subcommand
//! becomes one MCP tool. Dispatch is spawn-only.

use clap::{Args, Command, Subcommand};
use ecp_core::EcpError;
use ecp_mcp::server::{serve_stdio, EcpMcpServer};

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub action: McpAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Run stdio JSON-RPC MCP server.
    Serve,
    /// List tools that would be exposed by `serve`.
    Tools {
        /// Output format. Omit for the LLM-tuned default (text-tab); pass
        /// `--format json|toon` for the structured shapes.
        #[arg(long)]
        format: Option<String>,
    },
}

pub fn run(args: McpArgs, root_cmd: Command) -> Result<(), EcpError> {
    // Register the canonical repo identity with the MCP telemetry writer
    // BEFORE serving. Without this, telemetry.rs falls back to no-op
    // (the writer can't safely pick a key on its own — `repo_dir_name_for_cwd`
    // lives in ecp-cli and depends on `git_cache`). `Tools` action: still
    // call it so `init_repo_id` is consistent regardless of subcommand
    // (idempotent; no-op for any future re-entry).
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(repo_key) = crate::repo_identity::repo_dir_name_for_cwd(&cwd) {
            ecp_mcp::telemetry::init_repo_id(repo_key);
        }
    }

    let server =
        EcpMcpServer::new(&root_cmd).map_err(|e| EcpError::Output(format!("server init: {e}")))?;

    match args.action {
        McpAction::Tools { format } => {
            let tools = server.list_tools();

            match format.as_deref() {
                Some("json") | Some("toon") => {
                    if format.as_deref() == Some("toon") {
                        eprintln!(
                            "warning: toon renderer not yet integrated, falling back to json"
                        );
                    }
                    // The whole `DerivedTool`, not just name + description:
                    // `schema`, `positional_args`, `flag_args`, `prefix_args`
                    // and `subcmd_arg` are what a host needs to build the
                    // same argv the MCP server would.
                    let json = serde_json::to_string_pretty(tools)
                        .map_err(|e| EcpError::Output(format!("json: {e}")))?;
                    println!("{json}");
                }
                _ => {
                    // Default text: name<TAB>description per tool
                    for tool in tools {
                        println!("{}\t{}", tool.name, tool.description);
                    }
                }
            }
            Ok(())
        }
        McpAction::Serve => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| EcpError::Output(format!("tokio runtime: {e}")))?;
            rt.block_on(async move {
                serve_stdio(server)
                    .await
                    .map_err(|e| EcpError::Output(format!("serve_stdio: {e}")))
            })?;
            Ok(())
        }
    }
}
