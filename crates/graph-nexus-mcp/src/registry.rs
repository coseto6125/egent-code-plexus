//! Tool registry: types, inventory collection, name-derivation helpers.
//!
//! Each registered tool carries everything BOTH dispatch modes need:
//! - `handler` — daemon mode in-process call signature
//! - `subcommand` — spawn mode subprocess argument
//! - `name` / `description` / `schema` — MCP protocol metadata
//!
//! All four are filled by the `gnx_register_mcp_tool!` macro (Task 11).
//! At runtime, the MCP server iterates `inventory::iter::<GnxMcpTool>()`
//! and registers each.
//!
//! # schemars version note
//! This crate pins `schemars = "1"` (1.2.1), which ships `schemars::Schema`
//! as the top-level type. The 0.8.x path `schemars::schema::RootSchema` does
//! not exist in 1.x. `schema_for!` in schemars 1.x also returns `Schema`.
//!
//! # inventory::submit! const requirement
//! `inventory::submit!` stores values in statics inside a `const _: ()`
//! block, so every field initializer must be const-evaluable. Function
//! pointers (`fn() -> T`) satisfy this constraint regardless of whether the
//! body is const — that is why `name`, `description`, and `schema` are all
//! `fn() -> &'static str` / `fn() -> Schema` rather than bare values.

use graph_nexus_core::GnxError;
use schemars::Schema;
use serde_json::Value;

/// Engine handle abstracted at the boundary so this crate doesn't pull
/// the whole `graph-nexus-cli` Engine type into its public API.
/// Daemon mode wires this in `daemon.rs`; spawn mode never uses it.
pub trait EngineRef: Send + Sync {
    /// Path of the graph.bin currently loaded (for mtime-remap).
    fn graph_path(&self) -> &std::path::Path;

    /// Downcast to concrete Engine type if available (daemon mode).
    /// Returns None for tools called in spawn mode (no engine available).
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
}

pub struct GnxMcpTool {
    /// Returns the MCP tool name (e.g. `"gnx_context"`). Stored as a
    /// function pointer so `inventory::submit!` can place it in a static
    /// without requiring a const-fn body.
    pub name: fn() -> &'static str,
    /// Returns the human-readable description (the schemars schema_name).
    /// Same lazy fn-pointer pattern as `name`.
    pub description: fn() -> &'static str,
    /// schemars 1.x: `Schema` (was `RootSchema` in 0.8.x).
    pub schema: fn() -> Schema,
    /// Daemon mode: in-process handler.
    pub handler: fn(Value, &dyn EngineRef) -> Result<Value, GnxError>,
    /// Returns the subcommand string (last path segment of `module_path!()`)
    /// passed to spawn-mode `Command::new(self_exe).arg(_)`.
    pub subcommand: fn() -> &'static str,
}

inventory::collect!(GnxMcpTool);

/// Strip the leading `graph_nexus_cli::commands::` (or any prefix) and
/// prepend `gnx_`. The last `::` segment IS the subcommand identifier
/// in snake_case, which matches both the CLI subcommand name and the
/// desired MCP tool name (with prefix).
///
/// Callers should pass `module_path!()` or another `&'static str`; this
/// signature documents the calling convention. The output is leaked once
/// per registered tool — bounded by the finite set of MCP-exposed commands
/// (≤30).
pub fn derive_tool_name(module_path: &'static str) -> &'static str {
    let last = module_path.rsplit("::").next().unwrap_or(module_path);
    // Leak to 'static — module_path is itself 'static so this is sound.
    // We can't avoid the allocation entirely because we need a
    // formatted string, but each command-file's call only ever yields
    // one allocation for the binary's lifetime.
    Box::leak(format!("gnx_{last}").into_boxed_str())
}

pub fn derive_subcommand(module_path: &'static str) -> &'static str {
    module_path.rsplit("::").next().unwrap_or(module_path)
}
