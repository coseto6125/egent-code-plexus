//! "Other Code Agents" sub-menu — agents without a host-first entry, plus
//! the Codex CLI MCP-mode path (distinct from its native-tools patch). Claude
//! Code and Gemini CLI are fully covered by host-first scriptable commands
//! and no longer appear here.

pub(crate) mod claude_code;
pub mod cline_roo;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub(crate) mod gemini;
pub mod generic;
pub mod windsurf;

use crate::admin::menu::{self, select};
use dialoguer::theme::ColorfulTheme;
use ecp_core::EcpError;

const HOSTS: &[menu::Item<'_>] = &[
    ("Cursor", "Cursor editor — ~/.cursor/mcp.json"),
    (
        "Windsurf",
        "Windsurf editor — ~/.codeium/windsurf/mcp_config.json",
    ),
    (
        "Cline / Roo Code",
        "VS Code extensions — cline_mcp_settings.json",
    ),
    ("Codex CLI (MCP mode)", "Codex CLI — ~/.codex/config.toml"),
    ("Copilot", "GitHub Copilot — VS Code settings.json"),
    (
        "Generic (any MCP host)",
        "print stdio command + JSON snippet to paste",
    ),
    ("← Back", ""),
];

const ACTIONS: &[menu::Item<'_>] = &[
    ("install", "write the ecp MCP server entry to host config"),
    ("uninstall", "remove the ecp MCP entry from host config"),
    ("status", "show whether ecp is registered with this host"),
    ("← Back", ""),
];

/// Entry point called from `host_integration::run`.
pub fn run(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Other Code Agents — pick one", HOSTS)?;
        match choice {
            Some(0) => host_menu(
                theme,
                "Cursor",
                cursor::install,
                cursor::uninstall,
                cursor::status,
            )?,
            Some(1) => host_menu(
                theme,
                "Windsurf",
                windsurf::install,
                windsurf::uninstall,
                windsurf::status,
            )?,
            Some(2) => host_menu(
                theme,
                "Cline / Roo Code",
                cline_roo::install,
                cline_roo::uninstall,
                cline_roo::status,
            )?,
            Some(3) => host_menu(
                theme,
                "Codex CLI (MCP mode)",
                codex::install,
                codex::uninstall,
                codex::status,
            )?,
            Some(4) => host_menu(
                theme,
                "Copilot",
                copilot::install,
                copilot::uninstall,
                copilot::status,
            )?,
            Some(5) => host_menu(
                theme,
                "Generic MCP host",
                generic::install,
                generic::uninstall,
                generic::status,
            )?,
            Some(6) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

/// Per-host action menu: install / uninstall / status.
fn host_menu(
    theme: &ColorfulTheme,
    host_name: &str,
    install: fn(&ColorfulTheme),
    uninstall: fn(&ColorfulTheme),
    status: fn() -> crate::admin::status::HostStatus,
) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, &format!("{host_name} — action"), ACTIONS)?;
        match choice {
            Some(0) => install(theme),
            Some(1) => uninstall(theme),
            Some(2) => status().print(host_name),
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}
