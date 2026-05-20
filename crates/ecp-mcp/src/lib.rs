//! MCP server library backing `ecp mcp serve`.
//!
//! Tools are discovered by introspecting the ecp CLI's `clap::Command` tree
//! at server startup — every visible subcommand becomes one MCP tool.
//! Dispatch is spawn-only: each call invokes `ecp <subcommand> --flag val…`
//! in a subprocess and returns the stdout.
//!
//! See `schema.rs` for the clap → JSON-schema derivation, `server.rs` for
//! the rmcp stdio adapter, and `spawn.rs` for the subprocess invocation.

pub mod argv;
pub mod group;
pub mod peers;
pub mod schema;
pub mod server;
pub mod spawn;
