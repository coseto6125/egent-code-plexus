//! Scriptable Codex host integration commands for AI agents.

use crate::admin::host_integration::native::codex;
use crate::commands::admin::skill_fs::copy_dir_replace;
use clap::Subcommand;
use std::fs;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum CodexCommands {
    /// Install a Codex integration component.
    Install {
        #[command(subcommand)]
        component: CodexComponent,
    },
    /// Remove a Codex integration component.
    Uninstall {
        #[command(subcommand)]
        component: CodexComponent,
    },
    /// Show all Codex integration statuses.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum CodexComponent {
    /// Codex native tool scaffold for an openai/codex fork.
    NativeTools,
    /// Codex skills that teach LLMs when to use ecp beyond command help.
    Skills {
        #[command(subcommand)]
        target: SkillTarget,
    },
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum SkillTarget {
    /// Install every bundled Codex skill.
    All,
    /// Command-selection skill: when graph-aware ecp workflows beat plain grep or help output.
    Ecp,
    /// Review skill: when changed-code review should start from ecp impact and risk signals.
    Simplify,
}

pub fn run(command: CodexCommands) -> Result<(), ecp_core::EcpError> {
    match command {
        CodexCommands::Install { component } => install(component),
        CodexCommands::Uninstall { component } => uninstall(component),
        CodexCommands::Status => print_status(),
    }
}

fn install(component: CodexComponent) -> Result<(), ecp_core::EcpError> {
    match component {
        CodexComponent::NativeTools => {
            let path = codex::run_install()?;
            println!("Codex CLI native patch written to {}", path.display());
            println!(
                "Apply it in your openai/codex fork, then wire the generated tool into Codex's tool registry."
            );
        }
        CodexComponent::Skills { target } => install_skills(target)?,
    }
    Ok(())
}

fn uninstall(component: CodexComponent) -> Result<(), ecp_core::EcpError> {
    match component {
        CodexComponent::NativeTools => {
            let path = codex::run_uninstall()?;
            println!("Codex CLI native patch removed from {}", path.display());
        }
        CodexComponent::Skills { target } => uninstall_skills(target)?,
    }
    Ok(())
}

pub(crate) fn print_status() -> Result<(), ecp_core::EcpError> {
    codex::status().print("Codex CLI native-tools");
    for skill in [SkillTarget::Ecp, SkillTarget::Simplify] {
        let path = codex_skill_dir(skill);
        let label = format!("Codex CLI skill {}", skill.name());
        if path.join("SKILL.md").exists() {
            println!("  {label}: installed ({})", path.display());
        } else {
            println!("  {label}: missing");
        }
    }
    Ok(())
}

pub(crate) fn install_skills(target: SkillTarget) -> Result<(), ecp_core::EcpError> {
    for &skill in target.expand() {
        let src = source_skill_dir(skill)?;
        let dst = codex_skill_dir(skill);
        copy_dir_replace(&src, &dst)?;
        println!(
            "Codex CLI skill `{}` installed in {}",
            skill.name(),
            dst.display()
        );
    }
    Ok(())
}

pub(crate) fn uninstall_skills(target: SkillTarget) -> Result<(), ecp_core::EcpError> {
    for &skill in target.expand() {
        let dst = codex_skill_dir(skill);
        if dst.exists() {
            fs::remove_dir_all(&dst)?;
        }
        println!(
            "Codex CLI skill `{}` removed from {}",
            skill.name(),
            dst.display()
        );
    }
    Ok(())
}

fn source_skill_dir(skill: SkillTarget) -> Result<PathBuf, ecp_core::EcpError> {
    let path = std::env::current_dir()
        .map_err(|e| ecp_core::EcpError::Output(format!("current_dir: {e}")))?
        .join("skill_sample")
        .join("codex")
        .join(skill.name());
    if path.join("SKILL.md").exists() {
        Ok(path)
    } else {
        Err(ecp_core::EcpError::Output(format!(
            "missing bundled Codex skill `{}` at {}",
            skill.name(),
            path.display()
        )))
    }
}

fn codex_skill_dir(skill: SkillTarget) -> PathBuf {
    codex_home().join("skills").join(skill.name())
}

fn codex_home() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex")
}

impl SkillTarget {
    fn name(self) -> &'static str {
        match self {
            SkillTarget::All => "all",
            SkillTarget::Ecp => "ecp",
            SkillTarget::Simplify => "simplify",
        }
    }

    fn expand(self) -> &'static [SkillTarget] {
        match self {
            SkillTarget::All => &[SkillTarget::Ecp, SkillTarget::Simplify],
            SkillTarget::Ecp => &[SkillTarget::Ecp],
            SkillTarget::Simplify => &[SkillTarget::Simplify],
        }
    }
}
