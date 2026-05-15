//! MCP server library backing `gnx mcp serve`. Built around `inventory`
//! for zero-hardcode tool discovery; each gnx CLI command opts in via
//! `gnx_register_mcp_tool!`.

pub mod argv;
pub mod daemon;
pub mod macros;
pub mod registry;
pub mod server;
pub mod spawn;
