//! Stdio JSON-RPC MCP server. Spawn-mode only; tools are derived at
//! startup from the ecp CLI's `clap::Command` tree (see `schema.rs`).

use crate::schema::{ecp_tools, DerivedTool};
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
        let tools = ecp_tools(root);
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
        let ts = crate::telemetry::rfc3339_now();
        let start = std::time::Instant::now();
        let result =
            tokio::task::spawn_blocking(move || crate::spawn::run_spawn(&binary, &tool, &args))
                .await
                .map_err(|e| anyhow::anyhow!("spawn task: {e}"))?;
        let duration_ms = start.elapsed().as_millis() as u64;
        let ok = result.is_ok();
        let record = crate::telemetry::CallRecord {
            ts: &ts,
            tool: name,
            duration_ms,
            ok,
            source: "mcp",
            error_kind: None,
            subcommand: None,
            error_msg: None,
            version: None,
        };
        crate::telemetry::append(&record);
        result
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
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::ListToolsResult, rmcp::ErrorData>>
           + rmcp::service::MaybeSendFuture
           + '_ {
        let tools = self.0.rmcp_tools.to_vec();
        std::future::ready(Ok(list_tools_result(tools, context.protocol_version())))
    }

    fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::CallToolResponse, rmcp::ErrorData>>
           + rmcp::service::MaybeSendFuture
           + '_ {
        let server = Arc::clone(&self.0);
        async move {
            let args = match request.arguments {
                Some(map) => serde_json::Value::Object(map),
                None => serde_json::Value::Object(Default::default()),
            };
            // Spawn-mode dispatch runs the subcommand to completion in one shot, so
            // every call resolves as `Complete` — the MRTR `InputRequired` / `Task`
            // variants have no counterpart in this server.
            let result = match server.call_tool(&request.name, args).await {
                Ok(output) => {
                    rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                        output,
                    )])
                }
                Err(e) => {
                    rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                        e.to_string(),
                    )])
                }
            };
            Ok(result.into())
        }
    }
}

/// How long a peer may treat one `tools/list` response as fresh.
///
/// The tool set is derived once from the clap tree in `EcpMcpServer::new` and
/// never changes while the process lives, so any TTL up to the session length
/// would be honest. It stays short anyway: rmcp's client cache is keyed per
/// connection, and a spawn-mode session that outlives an `ecp` upgrade should
/// re-list rather than keep calling a subcommand the new binary dropped.
const TOOL_LIST_TTL_MS: u64 = 60_000;

/// Attach the SEP-2549 cache hints that protocol `2026-07-28` requires on
/// `tools/list`. A strict client on that version rejects a response missing
/// `ttlMs` / `cacheScope` outright, before any tool call. The tool set is
/// derived from the binary alone — no per-user data — so the scope is public.
///
/// Older peers predate both fields and keep the legacy wire shape, matching
/// how rmcp's own `#[tool_handler]` gates them.
fn list_tools_result(
    tools: Vec<rmcp::model::Tool>,
    protocol: Option<rmcp::model::ProtocolVersion>,
) -> rmcp::model::ListToolsResult {
    let result = rmcp::model::ListToolsResult::with_all_items(tools);
    match protocol {
        Some(version) if version >= rmcp::model::ProtocolVersion::V_2026_07_28 => result
            .with_ttl_ms(TOOL_LIST_TTL_MS)
            .with_cache_scope(rmcp::model::CacheScope::Public),
        _ => result,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ProtocolVersion;

    fn wire(protocol: Option<ProtocolVersion>) -> serde_json::Value {
        serde_json::to_value(list_tools_result(Vec::new(), protocol)).expect("serialize")
    }

    #[test]
    fn peer_on_2026_07_28_gets_the_required_cache_hints() {
        let json = wire(Some(ProtocolVersion::V_2026_07_28));
        assert_eq!(json["ttlMs"], serde_json::json!(TOOL_LIST_TTL_MS));
        assert_eq!(json["cacheScope"], serde_json::json!("public"));
    }

    /// The gate is `>=`, not `==`: a protocol newer than any this build knows
    /// about still requires the fields.
    #[test]
    fn peer_on_a_later_version_still_gets_the_cache_hints() {
        let future: ProtocolVersion =
            serde_json::from_value(serde_json::json!("2027-01-01")).expect("parse version");
        let json = wire(Some(future));
        assert_eq!(json["ttlMs"], serde_json::json!(TOOL_LIST_TTL_MS));
        assert_eq!(json["cacheScope"], serde_json::json!("public"));
    }

    #[test]
    fn legacy_peer_keeps_the_wire_shape_that_predates_the_fields() {
        for version in [
            ProtocolVersion::V_2025_11_25,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::V_2024_11_05,
        ] {
            let json = wire(Some(version.clone()));
            assert!(
                json.get("ttlMs").is_none() && json.get("cacheScope").is_none(),
                "cache hints leaked to legacy peer {version:?}: {json}"
            );
        }
    }

    #[test]
    fn unnegotiated_peer_keeps_the_legacy_wire_shape() {
        let json = wire(None);
        assert!(
            json.get("ttlMs").is_none() && json.get("cacheScope").is_none(),
            "cache hints emitted without a negotiated version: {json}"
        );
    }
}
