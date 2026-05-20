//! Stdio JSON-RPC MCP server. Spawn-mode only; tools are derived at
//! startup from the ecp CLI's `clap::Command` tree (see `schema.rs`).

use crate::schema::{enumerate_tools, DerivedTool};
use anyhow::{Context, Result};
use clap::Command;
use std::path::PathBuf;
use std::sync::Arc;

pub struct EcpMcpServer {
    /// Path to the current ecp binary; used to spawn subprocesses.
    pub self_exe: PathBuf,
    /// Tools derived from the clap tree at construction time.
    tools: Vec<DerivedTool>,
    /// Pre-built rmcp tool models; reused across every `tools/list` request.
    /// `rmcp::model::Tool` is internally `Arc`-backed, so `.to_vec()` over
    /// this slice is a cheap refcount bump per entry.
    rmcp_tools: Vec<rmcp::model::Tool>,
}

impl EcpMcpServer {
    /// Build a server whose tool set mirrors `root`'s visible subcommands.
    /// Self-binary is detected via `current_exe()`.
    pub fn new(root: &Command) -> Result<Self> {
        let self_exe =
            std::env::current_exe().context("locating current_exe for spawn dispatch")?;
        let mut tools = enumerate_tools(root);
        // Replace the opaque `ecp_peers` entry (which exposes no useful args)
        // with the three explicit peer sub-subcommand tools.
        tools.retain(|t| t.name != "ecp_peers");
        tools.extend(crate::peers::peer_tools());
        // `ecp group` is `#[command(hide = true)]` so enumerate_tools skips
        // it — without this manual injection, LLM clients cannot reach the
        // sub-subcommands at all. Discriminator: `subcmd`.
        tools.extend(crate::group::group_tools());
        let rmcp_tools = build_rmcp_tools(&tools);
        Ok(Self {
            self_exe,
            tools,
            rmcp_tools,
        })
    }

    pub fn list_tools(&self) -> &[DerivedTool] {
        &self.tools
    }

    /// Dispatch one MCP `tools/call`: spawn `ecp <subcommand>` with argv
    /// derived from the JSON args and return stdout.
    pub async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {name}"))?
            .clone();
        let binary = self.self_exe.clone();
        tokio::task::spawn_blocking(move || crate::spawn::run_spawn(&binary, &tool, &args))
            .await
            .map_err(|e| anyhow::anyhow!("spawn task: {e}"))?
    }
}

// ─── rmcp ServerHandler adapter ──────────────────────────────────────────────

struct RmcpHandler(Arc<EcpMcpServer>);

impl rmcp::ServerHandler for RmcpHandler {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ecp-mcp", env!("CARGO_PKG_VERSION")))
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::ListToolsResult, rmcp::ErrorData>>
           + rmcp::service::MaybeSendFuture
           + '_ {
        let tools = self.0.rmcp_tools.to_vec();
        std::future::ready(Ok(rmcp::model::ListToolsResult::with_all_items(tools)))
    }

    fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::CallToolResult, rmcp::ErrorData>>
           + rmcp::service::MaybeSendFuture
           + '_ {
        let server = Arc::clone(&self.0);
        async move {
            let args = match request.arguments {
                Some(map) => serde_json::Value::Object(map),
                None => serde_json::Value::Object(Default::default()),
            };
            match server.call_tool(&request.name, args).await {
                Ok(output) => Ok(rmcp::model::CallToolResult::success(vec![
                    rmcp::model::Content::text(output),
                ])),
                Err(e) => Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::Content::text(e.to_string()),
                ])),
            }
        }
    }
}

fn build_rmcp_tools(tools: &[DerivedTool]) -> Vec<rmcp::model::Tool> {
    tools
        .iter()
        .map(|t| {
            // `schema` is always built by `derive_tool` as `json!({type:"object", ...})`,
            // so `as_object()` is guaranteed to be `Some`. Using `.expect()` makes that
            // invariant explicit rather than carrying a dead fallback branch.
            let map = t
                .schema
                .as_object()
                .expect("DerivedTool::schema is always Value::Object")
                .clone();
            rmcp::model::Tool::new(t.name.clone(), t.description.clone(), Arc::new(map))
        })
        .collect()
}

pub async fn serve_stdio(server: EcpMcpServer) -> anyhow::Result<()> {
    let handler = RmcpHandler(Arc::new(server));
    let transport = rmcp::transport::stdio();
    let running = rmcp::serve_server(handler, transport).await?;
    running.waiting().await.ok();
    Ok(())
}
