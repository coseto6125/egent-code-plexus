//! Scriptable Claude Code host integration commands for AI agents.

use crate::admin::host_integration::mcp::claude_code as mcp_claude;
use crate::commands::admin::claude_code as hooks;
use crate::commands::admin::skill_fs::copy_dir_replace;
use clap::Subcommand;
use ecp_core::EcpError;
use std::fs;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ClaudeCommands {
    /// Install a Claude Code integration component.
    Install {
        #[command(subcommand)]
        component: ClaudeComponent,
    },
    /// Remove a Claude Code integration component.
    Uninstall {
        #[command(subcommand)]
        component: ClaudeComponent,
    },
    /// Show all Claude Code integration statuses.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum ClaudeComponent {
    /// settings.json event hooks (auto-reindex + context enrichment).
    Hooks {
        /// CSV of events (session-start, user-prompt-submit, pre-tool-use,
        /// post-tool-use). Omit to install/remove all four.
        #[arg(long)]
        events: Option<String>,
    },
    /// MCP server entry registered via `claude mcp add-json`.
    McpServer,
    /// Skill packs that teach Claude when to use ecp beyond command help.
    Skills {
        #[command(subcommand)]
        target: ClaudeSkillTarget,
    },
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum ClaudeSkillTarget {
    /// Install every bundled Claude skill.
    All,
    /// Review skill: when changed-code review should start from ecp impact and risk signals.
    Simplify,
}

pub fn run(command: ClaudeCommands) -> Result<(), EcpError> {
    match command {
        ClaudeCommands::Install { component } => install(component),
        ClaudeCommands::Uninstall { component } => uninstall(component),
        ClaudeCommands::Status => print_status(),
    }
}

pub(crate) fn install(component: ClaudeComponent) -> Result<(), EcpError> {
    match component {
        ClaudeComponent::Hooks { events } => {
            hooks::run_install_claude_code(events.as_deref(), None)
        }
        ClaudeComponent::McpServer => mcp_claude::install_scripted(),
        ClaudeComponent::Skills { target } => install_skills(target),
    }
}

pub(crate) fn uninstall(component: ClaudeComponent) -> Result<(), EcpError> {
    match component {
        ClaudeComponent::Hooks { events } => hooks::run_uninstall(hooks::UninstallHookArgs {
            claude_code: true,
            events,
            settings_path: None,
        }),
        ClaudeComponent::McpServer => mcp_claude::uninstall_scripted(),
        ClaudeComponent::Skills { target } => uninstall_skills(target),
    }
}

pub(crate) fn print_status() -> Result<(), EcpError> {
    hooks::run_status(hooks::StatusArgs {
        claude_code: true,
        settings_path: None,
    })?;
    mcp_claude::status().print("Claude Code mcp-server");
    for &skill in ClaudeSkillTarget::All.expand() {
        let path = claude_skill_dir(skill);
        let label = format!("Claude Code skill {}", skill.name());
        if path.join("SKILL.md").exists() {
            println!("  {label}: installed ({})", path.display());
        } else {
            println!("  {label}: missing");
        }
    }
    Ok(())
}

fn install_skills(target: ClaudeSkillTarget) -> Result<(), EcpError> {
    for &skill in target.expand() {
        let src = source_skill_dir(skill)?;
        let dst = claude_skill_dir(skill);
        copy_dir_replace(&src, &dst)?;
        println!(
            "Claude Code skill `{}` installed in {}",
            skill.name(),
            dst.display()
        );
    }
    Ok(())
}

fn uninstall_skills(target: ClaudeSkillTarget) -> Result<(), EcpError> {
    for &skill in target.expand() {
        let dst = claude_skill_dir(skill);
        if dst.exists() {
            fs::remove_dir_all(&dst)?;
        }
        println!(
            "Claude Code skill `{}` removed from {}",
            skill.name(),
            dst.display()
        );
    }
    Ok(())
}

fn source_skill_dir(skill: ClaudeSkillTarget) -> Result<PathBuf, EcpError> {
    let path = std::env::current_dir()
        .map_err(|e| EcpError::Output(format!("current_dir: {e}")))?
        .join("skill_sample")
        .join("claude")
        .join(skill.name());
    if path.join("SKILL.md").exists() {
        Ok(path)
    } else {
        Err(EcpError::Output(format!(
            "missing bundled Claude skill `{}` at {}",
            skill.name(),
            path.display()
        )))
    }
}

fn claude_skill_dir(skill: ClaudeSkillTarget) -> PathBuf {
    claude_home().join("skills").join(skill.name())
}

fn claude_home() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude")
}

impl ClaudeSkillTarget {
    fn name(self) -> &'static str {
        match self {
            ClaudeSkillTarget::All => "all",
            ClaudeSkillTarget::Simplify => "simplify",
        }
    }

    fn expand(self) -> &'static [ClaudeSkillTarget] {
        match self {
            ClaudeSkillTarget::All => &[ClaudeSkillTarget::Simplify],
            ClaudeSkillTarget::Simplify => &[ClaudeSkillTarget::Simplify],
        }
    }
}
