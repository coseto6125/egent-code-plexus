//! "Other Code Agents" sub-menu — the Codex CLI MCP-mode path (distinct from
//! its native-tools patch). Claude Code and Gemini CLI are fully covered by
//! host-first scriptable commands and no longer appear here. Cursor, Windsurf,
//! Cline/Roo, Copilot and generic hosts were print-only stubs and are gone.

pub(crate) mod claude_code;
pub mod codex;
pub(crate) mod gemini;

use crate::admin::menu::{self, select};
use dialoguer::theme::ColorfulTheme;
use ecp_core::EcpError;

const HOSTS: &[menu::Item<'_>] = &[
    ("Codex CLI (MCP mode)", "Codex CLI — ~/.codex/config.toml"),
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
                "Codex CLI (MCP mode)",
                codex::install,
                codex::uninstall,
                codex::status,
            )?,
            Some(1) | None => return Ok(()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hosts_menu_lists_only_working_installers() {
        let labels: Vec<&str> = HOSTS.iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["Codex CLI (MCP mode)", "← Back"]);
    }
}
