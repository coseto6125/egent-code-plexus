//! Scriptable Claude Code host integration commands for AI agents.
//!
//! Mirrors the Codex / Gemini host-first pattern (PRs #224, #225):
//! one subcommand per agent, with `install / uninstall / status`
//! actions and a `<component>` leaf for hooks / mcp-server / skills.

use crate::admin::host_integration::mcp::claude_code as mcp_claude;
use crate::commands::admin::claude_code as hooks;
use cgn_core::CgnError;
use clap::Subcommand;
use std::fs;
use std::path::{Path, PathBuf};

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
    /// Skill packs that teach Claude when to use cgn beyond command help.
    Skills {
        #[command(subcommand)]
        target: ClaudeSkillTarget,
    },
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum ClaudeSkillTarget {
    /// Install every bundled Claude skill.
    All,
    /// Review skill: when changed-code review should start from cgn impact and risk signals.
    Simplify,
}

pub fn run(command: ClaudeCommands) -> Result<(), CgnError> {
    match command {
        ClaudeCommands::Install { component } => install(component),
        ClaudeCommands::Uninstall { component } => uninstall(component),
        ClaudeCommands::Status => print_status(),
    }
}

pub(crate) fn install(component: ClaudeComponent) -> Result<(), CgnError> {
    match component {
        ClaudeComponent::Hooks { events } => {
            hooks::run_install_claude_code(events.as_deref(), None)
        }
        ClaudeComponent::McpServer => mcp_claude::install_scripted(),
        ClaudeComponent::Skills { target } => install_skills(target),
    }
}

pub(crate) fn uninstall(component: ClaudeComponent) -> Result<(), CgnError> {
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

pub(crate) fn print_status() -> Result<(), CgnError> {
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

fn install_skills(target: ClaudeSkillTarget) -> Result<(), CgnError> {
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

fn uninstall_skills(target: ClaudeSkillTarget) -> Result<(), CgnError> {
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

fn source_skill_dir(skill: ClaudeSkillTarget) -> Result<PathBuf, CgnError> {
    let path = std::env::current_dir()
        .map_err(|e| CgnError::Output(format!("current_dir: {e}")))?
        .join("skill_sample")
        .join("claude")
        .join(skill.name());
    if path.join("SKILL.md").exists() {
        Ok(path)
    } else {
        Err(CgnError::Output(format!(
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

fn copy_dir_replace(src: &Path, dst: &Path) -> Result<(), CgnError> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    fs::create_dir_all(dst)?;
    copy_dir_contents(src, dst)
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), CgnError> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            fs::create_dir_all(&dst_path)?;
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
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
