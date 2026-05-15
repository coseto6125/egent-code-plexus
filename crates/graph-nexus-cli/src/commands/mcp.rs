//! `gnx mcp` subcommand: serve | tools.

use clap::{Args, Subcommand};
use graph_nexus_core::GnxError;
use graph_nexus_mcp::server::{DispatchMode, GnxMcpServer};

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub action: McpAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Run stdio JSON-RPC MCP server.
    Serve {
        /// Use daemon mode (keep Engine mmap'd; mtime-remap before
        /// each call). Default is spawn mode.
        #[arg(long, default_value_t = false)]
        daemon: bool,
    },
    /// List tools that would be exposed by `serve`. Useful for debug
    /// and for the test invariant.
    Tools,
}

pub fn run(args: McpArgs) -> Result<(), GnxError> {
    match args.action {
        McpAction::Tools => {
            let server = GnxMcpServer::new(DispatchMode::Spawn)
                .map_err(|e| GnxError::Output(format!("server init: {e}")))?;
            for tool in server.list_tools() {
                println!("{}\t{}", (tool.name)(), (tool.description)());
            }
            Ok(())
        }
        McpAction::Serve { daemon } => {
            let mode = if daemon {
                DispatchMode::Daemon
            } else {
                DispatchMode::Spawn
            };
            let server = GnxMcpServer::new(mode)
                .map_err(|e| GnxError::Output(format!("server init: {e}")))?;
            if daemon {
                // Daemon mode wiring needs Engine handle resolution
                // (mtime-remap reload logic) which is being deferred
                // to subproject C (TUI install handler). For now,
                // daemon mode returns a friendly error.
                return Err(GnxError::InvalidArgument(
                    "daemon mode is not yet available; use spawn mode (omit --daemon)".into(),
                ));
            }
            // Spawn-mode stdio loop via rmcp.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| GnxError::Output(format!("tokio runtime: {e}")))?;
            rt.block_on(async move {
                graph_nexus_mcp::server::serve_stdio(server)
                    .await
                    .map_err(|e| GnxError::Output(format!("serve_stdio: {e}")))
            })?;
            Ok(())
        }
    }
}
