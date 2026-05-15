//! Stdio JSON-RPC MCP server scaffold. Wraps `rmcp::ServiceExt` and
//! dispatches tool calls via either spawn or daemon mode.
//!
//! The actual `rmcp::ServerHandler` trait impl + transport wiring
//! lives in `crates/graph-nexus-cli/src/commands/mcp.rs` (Task 16),
//! because daemon mode needs Engine handle resolution that's CLI-side.
//! This module provides the dispatch core that both modes share.

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

    /// Dispatch a single tool call. The server's stdio loop (Task 16)
    /// calls this for each `tools/call` JSON-RPC frame.
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
