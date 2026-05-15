//! Host-integration menu — mechanism-first (Native vs MCP).

pub mod mcp;
pub mod native;

use crate::admin::menu::select;
use dialoguer::theme::ColorfulTheme;
use graph_nexus_core::GnxError;

const MECHANISMS: &[&str] = &[
    "Native (no side-car; integrates into host's own tool registry)",
    "MCP (shared side-car for any MCP-capable host)",
    "← Back",
];

/// Entry point called from `admin::main_menu`.
pub fn run(theme: &ColorfulTheme) -> Result<(), GnxError> {
    loop {
        let choice = select(theme, "Bind tool to code agent", MECHANISMS)?;
        match choice {
            Some(0) => native::run(theme)?,
            Some(1) => mcp::run(theme)?,
            Some(2) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}
