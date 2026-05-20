//! Agent integration menu — host-first installers per supported code agent.

pub(crate) mod gemini_cli;
pub mod mcp;
pub mod native;

use crate::admin::menu::{self, select};
use crate::commands::admin::claude::{self, ClaudeComponent, ClaudeSkillTarget};
use crate::commands::admin::codex::{install_skills, print_status, uninstall_skills, SkillTarget};
use crate::commands::admin::gemini::{self, GeminiComponent};
use dialoguer::theme::ColorfulTheme;
use ecp_core::EcpError;

const MECHANISMS: &[menu::Item<'_>] = &[
    ("Claude Code", "install hooks, MCP server, and skills"),
    ("Codex CLI", "install native tools and skills"),
    ("Gemini CLI", "install native skill and MCP server"),
    (
        "Other Code Agents",
        "Cursor, Windsurf, Cline, Copilot, generic MCP host",
    ),
    ("← Back", ""),
];

/// Entry point called from `admin::main_menu`.
pub fn run(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Agent Integrations", MECHANISMS)?;
        match choice {
            Some(0) => claude_menu(theme)?,
            Some(1) => codex_menu(theme)?,
            Some(2) => gemini_menu(theme)?,
            Some(3) => mcp::run(theme)?,
            Some(4) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

const CLAUDE_ACTIONS: &[menu::Item<'_>] = &[
    ("install", "install a Claude Code integration component"),
    ("uninstall", "remove a Claude Code integration component"),
    ("status", "show all Claude Code integration statuses"),
    ("← Back", ""),
];

const CLAUDE_COMPONENTS: &[menu::Item<'_>] = &[
    ("hooks", "settings.json event hooks for auto-reindex"),
    (
        "mcp-server",
        "register ecp as an MCP server via `claude mcp`",
    ),
    ("skills", "Claude skill packs for graph-aware workflows"),
    ("← Back", ""),
];

const CLAUDE_SKILLS: &[menu::Item<'_>] = &[
    ("all", "install every bundled Claude skill"),
    (
        "simplify",
        "when reviews should start from ecp impact and risk signals",
    ),
    ("← Back", ""),
];

fn claude_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Claude Code — action", CLAUDE_ACTIONS)?;
        match choice {
            Some(0) => claude_install_menu(theme)?,
            Some(1) => claude_uninstall_menu(theme)?,
            Some(2) => claude::print_status()?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn claude_install_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Claude Code — install", CLAUDE_COMPONENTS)?;
        match choice {
            Some(0) => claude::install(ClaudeComponent::Hooks { events: None })?,
            Some(1) => claude::install(ClaudeComponent::McpServer)?,
            Some(2) => claude_install_skills_menu(theme)?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn claude_uninstall_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Claude Code — uninstall", CLAUDE_COMPONENTS)?;
        match choice {
            Some(0) => {
                let chosen = crate::commands::admin::claude_code::prompt_events_tui("uninstall")?;
                if chosen.is_empty() {
                    println!("No events selected — nothing to uninstall.");
                } else {
                    let events = Some(chosen.join(","));
                    claude::uninstall(ClaudeComponent::Hooks { events })?
                }
            }
            Some(1) => claude::uninstall(ClaudeComponent::McpServer)?,
            Some(2) => claude_uninstall_skills_menu(theme)?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn claude_install_skills_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Claude Code — install skills", CLAUDE_SKILLS)?;
        match choice {
            Some(0) => claude::install(ClaudeComponent::Skills {
                target: ClaudeSkillTarget::All,
            })?,
            Some(1) => claude::install(ClaudeComponent::Skills {
                target: ClaudeSkillTarget::Simplify,
            })?,
            Some(2) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn claude_uninstall_skills_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Claude Code — uninstall skills", CLAUDE_SKILLS)?;
        match choice {
            Some(0) => claude::uninstall(ClaudeComponent::Skills {
                target: ClaudeSkillTarget::All,
            })?,
            Some(1) => claude::uninstall(ClaudeComponent::Skills {
                target: ClaudeSkillTarget::Simplify,
            })?,
            Some(2) | None => return Ok(()),
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

const CODEX_COMPONENTS: &[menu::Item<'_>] = &[
    ("native-tools", "Codex native tool patch scaffold"),
    (
        "skills",
        "LLM workflow skills for when help output is not enough",
    ),
    ("← Back", ""),
];

const CODEX_SKILLS: &[menu::Item<'_>] = &[
    ("all", "install every bundled Codex skill"),
    (
        "ecp",
        "when graph-aware workflows beat plain grep or help output",
    ),
    (
        "simplify",
        "when reviews should start from ecp impact and risk signals",
    ),
    ("← Back", ""),
];

fn codex_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
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

fn codex_install_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Codex CLI — install", CODEX_COMPONENTS)?;
        match choice {
            Some(0) => native::codex::install(theme),
            Some(1) => codex_install_skills_menu(theme)?,
            Some(2) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn codex_uninstall_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Codex CLI — uninstall", CODEX_COMPONENTS)?;
        match choice {
            Some(0) => native::codex::uninstall(theme),
            Some(1) => codex_uninstall_skills_menu(theme)?,
            Some(2) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn codex_install_skills_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Codex CLI — install skills", CODEX_SKILLS)?;
        match choice {
            Some(0) => install_skills(SkillTarget::All)?,
            Some(1) => install_skills(SkillTarget::Ecp)?,
            Some(2) => install_skills(SkillTarget::Simplify)?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn codex_uninstall_skills_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Codex CLI — uninstall skills", CODEX_SKILLS)?;
        match choice {
            Some(0) => uninstall_skills(SkillTarget::All)?,
            Some(1) => uninstall_skills(SkillTarget::Ecp)?,
            Some(2) => uninstall_skills(SkillTarget::Simplify)?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

const GEMINI_ACTIONS: &[menu::Item<'_>] = &[
    ("install", "install a Gemini integration component"),
    ("uninstall", "remove a Gemini integration component"),
    ("status", "show all Gemini integration statuses"),
    ("← Back", ""),
];

const GEMINI_COMPONENTS: &[menu::Item<'_>] = &[
    ("native-skill", "link the ecp skill into Gemini CLI"),
    ("mcp-server", "register ecp as an MCP server in Gemini CLI"),
    ("← Back", ""),
];

fn gemini_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Gemini CLI — action", GEMINI_ACTIONS)?;
        match choice {
            Some(0) => gemini_install_menu(theme)?,
            Some(1) => gemini_uninstall_menu(theme)?,
            Some(2) => gemini::print_status()?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn gemini_install_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Gemini CLI — install", GEMINI_COMPONENTS)?;
        match choice {
            Some(0) => gemini::install(GeminiComponent::NativeSkill)?,
            Some(1) => gemini::install(GeminiComponent::McpServer)?,
            Some(2) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn gemini_uninstall_menu(theme: &ColorfulTheme) -> Result<(), EcpError> {
    loop {
        let choice = select(theme, "Gemini CLI — uninstall", GEMINI_COMPONENTS)?;
        match choice {
            Some(0) => gemini::uninstall(GeminiComponent::NativeSkill)?,
            Some(1) => gemini::uninstall(GeminiComponent::McpServer)?,
            Some(2) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_integrations_menu_is_host_first() {
        let labels: Vec<&str> = MECHANISMS.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            vec![
                "Claude Code",
                "Codex CLI",
                "Gemini CLI",
                "Other Code Agents",
                "← Back",
            ]
        );
    }
}
