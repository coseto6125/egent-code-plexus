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
        /// Output format. Omit for the LLM-tuned default (text-tab); pass
        /// `--format json|toon` for the structured shapes.
        #[arg(long)]
        format: Option<String>,
    },
}

pub fn run(args: McpArgs, root_cmd: Command) -> Result<(), GnxError> {
    let server =
        GnxMcpServer::new(&root_cmd).map_err(|e| GnxError::Output(format!("server init: {e}")))?;

    match args.action {
        McpAction::Tools { format } => {
            let tools = server.list_tools();

            match format.as_deref() {
                Some("json") | Some("toon") => {
                    if format.as_deref() == Some("toon") {
                        eprintln!("warning: toon renderer not yet integrated, falling back to json");
                    }
                    let tool_infos: Vec<ToolInfo> = tools
                        .iter()
                        .map(|t| ToolInfo {
                            name: t.name.clone(),
                            description: t.description.clone(),
                        })
                        .collect();
                    let json = serde_json::to_string_pretty(&tool_infos)
                        .map_err(|e| GnxError::Output(format!("json: {e}")))?;
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
