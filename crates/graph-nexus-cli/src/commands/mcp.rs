//! `gnx mcp` subcommand: serve | tools.
//!
//! Tools are derived from the gnx CLI's `clap::Command` tree (see
//! `graph-nexus-mcp::schema`). Every visible non-hidden subcommand
//! becomes one MCP tool. Dispatch is spawn-only.

use clap::{Args, Command, Subcommand};
use graph_nexus_core::GnxError;
use graph_nexus_mcp::server::{serve_stdio, GnxMcpServer};
use serde::Serialize;

#[derive(Serialize, Debug)]
struct ToolInfo {
    name: String,
    description: String,
}

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
        /// Output format: text (default) | json | toon
        #[arg(long, default_value = "text")]
        format: String,
    },
}

pub fn run(args: McpArgs, root_cmd: Command) -> Result<(), GnxError> {
    let server =
        GnxMcpServer::new(&root_cmd).map_err(|e| GnxError::Output(format!("server init: {e}")))?;

    match args.action {
        McpAction::Tools { format } => {
            let tools = server.list_tools();
            let tool_infos: Vec<ToolInfo> = tools
                .iter()
                .map(|t| ToolInfo {
                    name: t.name.clone(),
                    description: t.description.clone(),
                })
                .collect();

            match format.as_str() {
                "json" => {
                    let json = serde_json::to_string_pretty(&tool_infos)
                        .map_err(|e| GnxError::Output(format!("json: {e}")))?;
                    println!("{json}");
                }
                "toon" => {
                    let value = serde_json::to_value(&tool_infos)
                        .map_err(|e| GnxError::Output(format!("toon serialize: {e}")))?;
                    // TODO: etoon integration pending; fallback to JSON for now
                    let json = serde_json::to_string_pretty(&value)
                        .map_err(|e| GnxError::Output(format!("toon json: {e}")))?;
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
                .map_err(|e| GnxError::Output(format!("tokio runtime: {e}")))?;
            rt.block_on(async move {
                serve_stdio(server)
                    .await
                    .map_err(|e| GnxError::Output(format!("serve_stdio: {e}")))
            })?;
            Ok(())
        }
    }
}
