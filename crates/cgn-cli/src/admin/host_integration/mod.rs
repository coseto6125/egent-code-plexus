//! Agent integration menu — MCP, Native, and hook installers.

pub mod mcp;
pub mod native;

use crate::admin::menu::{self, select};
use crate::commands::admin::{claude_code, install_hook};
use dialoguer::theme::ColorfulTheme;
use cgn_core::CgnError;

const MECHANISMS: &[menu::Item<'_>] = &[
    ("MCP", "shared side-car for any MCP-capable host"),
    (
        "Native",
        "no side-car; integrates into host's own tool registry",
    ),
    ("Hooks", "shell hooks (Claude Code) for auto-reindex on edits"),
    ("← Back", ""),
];

/// Entry point called from `admin::main_menu`.
pub fn run(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Agent Integrations", MECHANISMS)?;
        match choice {
            Some(0) => mcp::run(theme)?,
            Some(1) => native::run(theme)?,
            Some(2) => hooks_menu(theme)?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

const HOOK_HOSTS: &[menu::Item<'_>] = &[
    ("Claude Code hooks", "PreToolUse + PostToolUse for auto-reindex"),
    ("← Back", ""),
];
const HOOK_ACTIONS: &[menu::Item<'_>] = &[
    ("install", "write hook entries to ~/.claude/settings.json"),
    ("uninstall", "remove cgn hook entries from the host settings"),
    ("status", "show whether cgn hooks are registered"),
    ("← Back", ""),
];

fn hooks_menu(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Hooks", HOOK_HOSTS)?;
        match choice {
            Some(0) => claude_code_hooks_menu(theme)?,
            Some(1) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn claude_code_hooks_menu(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Claude Code hooks — action", HOOK_ACTIONS)?;
        match choice {
            Some(0) => install_hook::run(install_hook::InstallHookArgs {
                force: false,
                no_chain: false,
                claude_code: true,
                events: None,
                settings_path: None,
            })?,
            Some(1) => claude_code::run_uninstall(claude_code::UninstallHookArgs {
                claude_code: true,
                events: None,
                settings_path: None,
            })?,
            Some(2) => claude_code::run_status(claude_code::StatusArgs {
                claude_code: true,
                settings_path: None,
            })?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_integrations_menu_groups_mechanisms_and_hooks() {
        let labels: Vec<&str> = MECHANISMS.iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, vec!["MCP", "Native", "Hooks", "← Back"]);
    }
}
