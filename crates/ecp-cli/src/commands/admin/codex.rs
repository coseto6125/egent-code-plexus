//! Scriptable Codex host integration commands for AI agents.

use crate::admin::host_integration::native::codex;
use crate::commands::admin::skill_fs::copy_dir_replace;
use crate::commands::admin::skill_source::{resolve, EmbeddedTree};
use clap::{Args, Subcommand};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    NativeTools(NativeToolsArgs),
    /// Codex skills that teach LLMs when to use ecp beyond command help.
    Skills {
        #[command(subcommand)]
        target: SkillTarget,
    },
}

#[derive(Args, Debug, Default)]
pub struct NativeToolsArgs {
    /// After writing the patch, run `gh repo fork openai/codex --clone` to
    /// the target directory and `git apply` the patch. Requires `gh` to be
    /// authenticated. Skips the fork step (still applies the patch) if the
    /// target directory already contains a git checkout.
    #[arg(long = "auto-fork")]
    pub auto_fork: bool,

    /// Override the fork checkout directory. Defaults to
    /// `~/.config/ecp/host-integration/codex-fork` (or `$ECP_CODEX_FORK_DIR`
    /// if set). Only honored with `--auto-fork`.
    #[arg(long = "fork-dir")]
    pub fork_dir: Option<PathBuf>,
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
        CodexComponent::NativeTools(args) => {
            if args.auto_fork {
                println!("Codex CLI native-tools: --auto-fork ignored");
            }
            println!("Codex CLI native-tools: {}", codex::pending_message());
            return Ok(());
        }
        CodexComponent::Skills { target } => install_skills(target)?,
    }
    Ok(())
}

fn uninstall(component: CodexComponent) -> Result<(), ecp_core::EcpError> {
    match component {
        CodexComponent::NativeTools(_) => {
            let path = codex::run_uninstall()?;
            println!("Codex CLI native patch removed from {}", path.display());
        }
        CodexComponent::Skills { target } => uninstall_skills(target)?,
    }
    Ok(())
}

/// `~/.config/ecp/host-integration/codex-fork/` unless `$ECP_CODEX_FORK_DIR`
/// or an explicit `--fork-dir` overrides.
// TODO(native-tools): re-enable when native-tools writes a full Codex registry
// patch instead of an adapter-only scaffold.
#[allow(dead_code)]
fn resolve_fork_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(env) = std::env::var_os("ECP_CODEX_FORK_DIR") {
        return PathBuf::from(env);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("ecp").join("host-integration").join("codex-fork")
}

/// `gh repo fork openai/codex --clone` to `fork_dir` (skips fork if the dir
/// already holds a git checkout), then `git -C <fork_dir> apply <patch>`.
// TODO(native-tools): re-enable when `install native-tools --auto-fork` can
// apply dependency + registry hunks for a concrete Codex checkout.
#[allow(dead_code)]
fn auto_fork_and_apply(patch: &Path, fork_dir: &Path) -> Result<(), ecp_core::EcpError> {
    let auth = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map_err(|e| ecp_core::EcpError::Output(format!("spawn `gh auth status`: {e}")))?;
    if !auth.status.success() {
        return Err(ecp_core::EcpError::Output(format!(
            "`gh auth status` failed — install + authenticate gh CLI first:\n{}",
            String::from_utf8_lossy(&auth.stderr).trim()
        )));
    }

    let already_cloned = fork_dir.join(".git").exists();
    if already_cloned {
        println!(
            "Reusing existing Codex fork checkout at {} (skip fork step)",
            fork_dir.display()
        );
    } else {
        if let Some(parent) = fork_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        let parent = fork_dir.parent().unwrap_or(Path::new("."));
        let leaf = fork_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "codex".into());
        // `gh repo fork --clone --` accepts trailing git args. `--` passes the
        // target directory to git so clone lands at the exact requested path.
        let fork = Command::new("gh")
            .args([
                "repo",
                "fork",
                "openai/codex",
                "--clone",
                "--remote=true",
                "--",
                &leaf,
            ])
            .current_dir(parent)
            .output()
            .map_err(|e| ecp_core::EcpError::Output(format!("spawn `gh repo fork`: {e}")))?;
        if !fork.status.success() {
            return Err(ecp_core::EcpError::Output(format!(
                "`gh repo fork openai/codex` failed:\n{}",
                String::from_utf8_lossy(&fork.stderr).trim()
            )));
        }
        println!("Forked openai/codex → {}", fork_dir.display());
    }

    let apply = Command::new("git")
        .args(["-C", &fork_dir.to_string_lossy()])
        .args(["apply", &patch.to_string_lossy()])
        .output()
        .map_err(|e| ecp_core::EcpError::Output(format!("spawn `git apply`: {e}")))?;
    if !apply.status.success() {
        return Err(ecp_core::EcpError::Output(format!(
            "`git apply {}` failed in {}:\n{}",
            patch.display(),
            fork_dir.display(),
            String::from_utf8_lossy(&apply.stderr).trim()
        )));
    }
    println!(
        "Patch applied in {}. Inspect with `git -C {} diff`, then commit + push to your fork.",
        fork_dir.display(),
        fork_dir.display()
    );
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
    let cwd = std::env::current_dir()
        .map_err(|e| ecp_core::EcpError::Output(format!("current_dir: {e}")))?;
    for &skill in target.expand() {
        let src = resolve(EmbeddedTree::CodexSample(skill.name()), &cwd)?;
        let dst = codex_skill_dir(skill);
        copy_dir_replace(src.path(), &dst)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture HOME-state lock so env-var manipulation in tests doesn't race
    /// when `cargo test` runs them in parallel. `std::env::set_var` is
    /// process-global state.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_fork_dir_explicit_wins_over_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let explicit = PathBuf::from("/tmp/explicit-codex");
        let prev = std::env::var_os("ECP_CODEX_FORK_DIR");
        std::env::set_var("ECP_CODEX_FORK_DIR", "/tmp/env-set");
        let resolved = resolve_fork_dir(Some(&explicit));
        assert_eq!(resolved, explicit);
        match prev {
            Some(v) => std::env::set_var("ECP_CODEX_FORK_DIR", v),
            None => std::env::remove_var("ECP_CODEX_FORK_DIR"),
        }
    }

    #[test]
    fn resolve_fork_dir_env_wins_over_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var_os("ECP_CODEX_FORK_DIR");
        std::env::set_var("ECP_CODEX_FORK_DIR", "/tmp/env-fork");
        let resolved = resolve_fork_dir(None);
        assert_eq!(resolved, PathBuf::from("/tmp/env-fork"));
        match prev {
            Some(v) => std::env::set_var("ECP_CODEX_FORK_DIR", v),
            None => std::env::remove_var("ECP_CODEX_FORK_DIR"),
        }
    }

    #[test]
    fn resolve_fork_dir_default_under_config() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_env = std::env::var_os("ECP_CODEX_FORK_DIR");
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        std::env::remove_var("ECP_CODEX_FORK_DIR");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("HOME", "/tmp/fake-home");
        let resolved = resolve_fork_dir(None);
        assert_eq!(
            resolved,
            PathBuf::from("/tmp/fake-home/.config/ecp/host-integration/codex-fork")
        );
        match prev_env {
            Some(v) => std::env::set_var("ECP_CODEX_FORK_DIR", v),
            None => std::env::remove_var("ECP_CODEX_FORK_DIR"),
        }
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
