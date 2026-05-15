//! Stdio JSON-RPC MCP server scaffold. Wraps `rmcp::ServiceExt` and
//! dispatches tool calls via either spawn or daemon mode.
//!
//! The `serve_stdio` function wires `GnxMcpServer` to rmcp's stdio
//! transport using the `ServerHandler` trait. Both spawn and daemon
//! dispatch modes are supported; daemon wiring for the Engine handle
//! reload lands with subproject C.

use crate::registry::{EngineRef, GnxMcpTool};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub enum DispatchMode {
    /// Default: spawn `gnx <subcommand>` per call.
    Spawn,
    /// Opt-in: keep Engine mmap'd; mtime-remap before each call.
    Daemon,
}

/// Owned state held by the server in daemon mode. Holds the loaded
/// Engine (via `EngineRef`) + the path it was loaded from + the mtime
/// at load time (used by `crate::daemon::needs_remap`).
pub struct DaemonState {
    pub engine_path: PathBuf,
    pub loaded_at: std::time::SystemTime,
    /// The actual Engine handle, wrapped so this crate doesn't depend
    /// on graph-nexus-cli's concrete Engine type. CLI side (Task 16)
    /// will provide a real impl.
    pub engine: Box<dyn EngineRef>,
}

impl EngineRef for DaemonState {
    fn graph_path(&self) -> &std::path::Path {
        &self.engine_path
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        // Defer to the wrapped engine — DaemonState itself is the
        // wrapper, downcasters want the inner Engine.
        self.engine.as_any()
    }
}

pub struct GnxMcpServer {
    mode: DispatchMode,
    daemon_state: Option<Arc<std::sync::Mutex<DaemonState>>>,
    /// Path to the current gnx binary (used by spawn mode).
    pub self_exe: PathBuf,
}

impl GnxMcpServer {
    pub fn new(mode: DispatchMode) -> Result<Self> {
        let self_exe = std::env::current_exe()
            .context("locating current_exe for spawn dispatch")?;
        Ok(Self {
            mode,
            daemon_state: None,
            self_exe,
        })
    }

    pub fn with_daemon_state(mut self, state: DaemonState) -> Self {
        self.daemon_state = Some(Arc::new(std::sync::Mutex::new(state)));
        self
    }

    pub fn mode(&self) -> DispatchMode {
        self.mode
    }

    /// Enumerate all tools registered via inventory at link time.
    pub fn list_tools(&self) -> Vec<&'static GnxMcpTool> {
        inventory::iter::<GnxMcpTool>().collect()
    }

    /// Dispatch a single tool call. Called from the rmcp `ServerHandler`
    /// impl for each `tools/call` JSON-RPC frame.
    pub async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<String> {
        let tool = self
            .list_tools()
            .into_iter()
            .find(|t| (t.name)() == name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {name}"))?;
        match self.mode {
            DispatchMode::Spawn => {
                crate::spawn::run_spawn(&self.self_exe, (tool.subcommand)(), &args)
            }
            DispatchMode::Daemon => {
                let state_arc = self
                    .daemon_state
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("daemon mode requires DaemonState"))?;
                let mut state = state_arc.lock().unwrap();
                // mtime-remap probe (cheap stat). On Err we leave the
                // existing engine in place rather than aborting — the
                // caller will get whatever the (possibly stale) engine
                // returns, which is safer than 503-ing the whole tool.
                if let Ok(true) = crate::daemon::needs_remap(&state.engine_path, state.loaded_at) {
                    // Refresh loaded_at. Actual Engine re-load is the
                    // responsibility of the CLI-side daemon wiring
                    // (Task 16) which has the concrete Engine type.
                    state.loaded_at = std::fs::metadata(&state.engine_path)?.modified()?;
                }
                let value = (tool.handler)(args, &*state)
                    .map_err(|e| anyhow::anyhow!("tool handler: {e}"))?;
                Ok(serde_json::to_string(&value)?)
            }
        }
    }
}

// ─── rmcp ServerHandler adapter ──────────────────────────────────────────────

/// Thin wrapper that implements `rmcp::ServerHandler` around `GnxMcpServer`.
/// Kept in this module so `commands/mcp.rs` can call `serve_stdio` without
/// depending on rmcp directly.
struct RmcpHandler(Arc<GnxMcpServer>);

impl rmcp::ServerHandler for RmcpHandler {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new(
            "graph-nexus-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<
        Output = Result<rmcp::model::ListToolsResult, rmcp::ErrorData>,
    > + rmcp::service::MaybeSendFuture + '_ {
        let tools = build_rmcp_tools(&self.0);
        std::future::ready(Ok(rmcp::model::ListToolsResult::with_all_items(tools)))
    }

    fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<
        Output = Result<rmcp::model::CallToolResult, rmcp::ErrorData>,
    > + rmcp::service::MaybeSendFuture + '_ {
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

/// Convert inventory-registered `GnxMcpTool`s to `rmcp::model::Tool`s.
///
/// `schemars::Schema` (1.x) serialises cleanly to a JSON object via
/// `serde_json::to_value`, which is what `Arc<JsonObject>` expects.
fn build_rmcp_tools(server: &GnxMcpServer) -> Vec<rmcp::model::Tool> {
    server
        .list_tools()
        .into_iter()
        .map(|t| {
            let schema_val = serde_json::to_value((t.schema)())
                .unwrap_or_else(|_| serde_json::json!({"type": "object"}));
            let input_schema: Arc<rmcp::model::JsonObject> = Arc::new(match schema_val {
                serde_json::Value::Object(map) => map,
                other => {
                    let mut m = serde_json::Map::new();
                    m.insert("description".into(), other);
                    m
                }
            });
            rmcp::model::Tool::new((t.name)(), (t.description)(), input_schema)
        })
        .collect()
}

/// Stdio JSON-RPC MCP server loop using rmcp 1.7.
///
/// Reads JSON-RPC frames from stdin, writes responses to stdout.
/// Blocks until the host disconnects (EOF on stdin).
///
/// Transport: `rmcp::transport::stdio()` → `(tokio::io::Stdin, tokio::io::Stdout)`.
/// Protocol: `rmcp::serve_server(handler, transport).await?.waiting().await`.
pub async fn serve_stdio(server: GnxMcpServer) -> anyhow::Result<()> {
    let handler = RmcpHandler(Arc::new(server));
    let transport = rmcp::transport::stdio();
    let running = rmcp::serve_server(handler, transport).await?;
    running.waiting().await.ok();
    Ok(())
}
