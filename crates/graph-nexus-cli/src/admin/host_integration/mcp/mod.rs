//! MCP sub-menu — pick a host, then install / uninstall / status.

pub mod claude_code;
pub mod cline_roo;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod generic;
pub mod windsurf;

use crate::admin::menu::{self, select};
use dialoguer::theme::ColorfulTheme;
use graph_nexus_core::GnxError;

const HOSTS: &[menu::Item<'_>] = &[
    ("Claude Code", "Anthropic CLI — ~/.claude/settings.json"),
    ("Cursor", "Cursor editor — ~/.cursor/mcp.json"),
    ("Windsurf", "Windsurf editor — ~/.codeium/windsurf/mcp_config.json"),
    ("Cline / Roo Code", "VS Code extensions — cline_mcp_settings.json"),
    ("Codex CLI", "Codex CLI in MCP mode — ~/.codex/config.toml"),
    ("Gemini CLI", "Gemini CLI in MCP mode — ~/.gemini/settings.json"),
    ("Copilot", "GitHub Copilot — VS Code settings.json"),
    ("Generic (any MCP host)", "print stdio command + JSON snippet to paste"),
    ("← Back", ""),
];

const ACTIONS: &[menu::Item<'_>] = &[
    ("install", "write the gnx MCP server entry to host config"),
    ("uninstall", "remove the gnx MCP entry from host config"),
    ("status", "show whether gnx is registered with this host"),
    ("← Back", ""),
];

/// Entry point called from `host_integration::run`.
pub fn run(theme: &ColorfulTheme) -> Result<(), GnxError> {
    loop {
        let choice = select(theme, "MCP — pick a host", HOSTS)?;
        match choice {
            Some(0) => host_menu(
                theme,
                "Claude Code",
                claude_code::install,
                claude_code::uninstall,
                claude_code::status,
            )?,
            Some(1) => host_menu(
                theme,
                "Cursor",
                cursor::install,
                cursor::uninstall,
                cursor::status,
            )?,
            Some(2) => host_menu(
                theme,
                "Windsurf",
                windsurf::install,
                windsurf::uninstall,
                windsurf::status,
            )?,
            Some(3) => host_menu(
                theme,
                "Cline / Roo Code",
                cline_roo::install,
                cline_roo::uninstall,
                cline_roo::status,
            )?,
            Some(4) => host_menu(
                theme,
                "Codex CLI (MCP)",
                codex::install,
                codex::uninstall,
                codex::status,
            )?,
            Some(5) => host_menu(
                theme,
                "Gemini CLI (MCP)",
                gemini::install,
                gemini::uninstall,
                gemini::status,
            )?,
            Some(6) => host_menu(
                theme,
                "Copilot",
                copilot::install,
                copilot::uninstall,
                copilot::status,
            )?,
            Some(7) => host_menu(
                theme,
                "Generic MCP host",
                generic::install,
                generic::uninstall,
                generic::status,
            )?,
            Some(8) | None => return Ok(()),
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
) -> Result<(), GnxError> {
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
