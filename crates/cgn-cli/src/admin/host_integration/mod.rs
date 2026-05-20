//! Agent integration menu — host-first installers plus MCP / native / hooks.

pub mod mcp;
pub mod native;

use crate::admin::menu::{self, select};
use crate::commands::admin::codex::{install_skills, print_status, uninstall_skills, SkillTarget};
use crate::commands::admin::{claude_code, install_hook};
use cgn_core::CgnError;
use dialoguer::theme::ColorfulTheme;

const MECHANISMS: &[menu::Item<'_>] = &[
    ("Codex CLI", "install native tools, hooks, and skills"),
    ("MCP", "shared side-car for any MCP-capable host"),
    (
        "Native",
        "no side-car; integrates into host's own tool registry",
    ),
    (
        "Hooks",
        "shell hooks (Claude Code) for auto-reindex on edits",
    ),
    ("← Back", ""),
];

/// Entry point called from `admin::main_menu`.
pub fn run(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Agent Integrations", MECHANISMS)?;
        match choice {
            Some(0) => codex_menu(theme)?,
            Some(1) => mcp::run(theme)?,
            Some(2) => native::run(theme)?,
            Some(3) => hooks_menu(theme)?,
            Some(4) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

const CODEX_ACTIONS: &[menu::Item<'_>] = &[
    ("install", "install a Codex integration component"),
    ("uninstall", "remove a Codex integration component"),
    ("status", "show all Codex integration statuses"),
    ("← Back", ""),
];

const CODEX_INSTALL_COMPONENTS: &[menu::Item<'_>] = &[
    ("native-tools", "write the Codex native tool patch scaffold"),
    (
        "skills",
        "install LLM workflow skills for when help output is not enough",
    ),
    ("← Back", ""),
];

const CODEX_SKILLS: &[menu::Item<'_>] = &[
    ("all", "install every bundled Codex skill"),
    (
        "cgn",
        "when graph-aware workflows beat plain grep or help output",
    ),
    (
        "simplify",
        "when reviews should start from cgn impact and risk signals",
    ),
    ("← Back", ""),
];

fn codex_menu(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Codex CLI — action", CODEX_ACTIONS)?;
        match choice {
            Some(0) => codex_install_menu(theme)?,
            Some(1) => codex_uninstall_menu(theme)?,
            Some(2) => print_status()?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn codex_install_menu(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Codex CLI — install", CODEX_INSTALL_COMPONENTS)?;
        match choice {
            Some(0) => native::codex::install(theme),
            Some(1) => codex_install_skills_menu(theme)?,
            Some(2) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn codex_uninstall_menu(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Codex CLI — uninstall", CODEX_INSTALL_COMPONENTS)?;
        match choice {
            Some(0) => native::codex::uninstall(theme),
            Some(1) => codex_uninstall_skills_menu(theme)?,
            Some(2) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn codex_install_skills_menu(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Codex CLI — install skills", CODEX_SKILLS)?;
        match choice {
            Some(0) => install_skills(SkillTarget::All)?,
            Some(1) => install_skills(SkillTarget::Cgn)?,
            Some(2) => install_skills(SkillTarget::Simplify)?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn codex_uninstall_skills_menu(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Codex CLI — uninstall skills", CODEX_SKILLS)?;
        match choice {
            Some(0) => uninstall_skills(SkillTarget::All)?,
            Some(1) => uninstall_skills(SkillTarget::Cgn)?,
            Some(2) => uninstall_skills(SkillTarget::Simplify)?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

const HOOK_HOSTS: &[menu::Item<'_>] = &[
    (
        "Claude Code hooks",
        "PreToolUse + PostToolUse for auto-reindex",
    ),
    ("← Back", ""),
];
const HOOK_ACTIONS: &[menu::Item<'_>] = &[
    ("install", "write hook entries to ~/.claude/settings.json"),
    (
        "uninstall",
        "remove cgn hook entries from the host settings",
    ),
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
        assert_eq!(
            labels,
            vec!["Codex CLI", "MCP", "Native", "Hooks", "← Back"]
        );
    }
}
