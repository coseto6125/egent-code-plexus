//! Scriptable Claude Code host integration commands for AI agents.

use crate::admin::host_integration::mcp::claude_code as mcp_claude;
use crate::commands::admin::claude_code as hooks;
use crate::commands::admin::skill_fs::{copy_dir_replace, skill_diff};
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
        /// Which skill(s) to install. Defaults to all.
        #[arg(value_enum, default_value_t = ClaudeSkillTarget::All)]
        target: ClaudeSkillTarget,
        /// Print the diff against the installed copy without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum ClaudeSkillTarget {
    /// Install every bundled Claude skill.
    All,
    /// Command-selection skill: when graph-aware ecp workflows beat plain grep
    /// or help output. Sourced from the canonical `docs/skills/ecp/`.
    Ecp,
    /// Review skill: when changed-code review should start from ecp impact
    /// and risk signals.
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
        ClaudeComponent::Skills { target, dry_run } => install_skills(target, dry_run),
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
        ClaudeComponent::Skills { target, .. } => uninstall_skills(target),
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

pub(crate) fn install_skills(target: ClaudeSkillTarget, dry_run: bool) -> Result<(), EcpError> {
    let cwd = std::env::current_dir().map_err(|e| EcpError::Output(format!("current_dir: {e}")))?;
    install_skills_at(target, dry_run, &cwd, &claude_home())
}

/// `install_skills` with explicit source-root (`cwd`) and install-root
/// (`claude_home`) so E2E tests drive the whole diff+copy flow against
/// tempdirs without mutating process-global cwd / HOME (which race parallel
/// tests).
pub(crate) fn install_skills_at(
    target: ClaudeSkillTarget,
    dry_run: bool,
    cwd: &std::path::Path,
    claude_home: &std::path::Path,
) -> Result<(), EcpError> {
    for &skill in target.expand() {
        let src = source_skill_dir_at(skill, cwd);
        if !src.join("SKILL.md").exists() {
            return Err(EcpError::Output(format!(
                "missing bundled Claude skill `{}` at {}",
                skill.name(),
                src.display()
            )));
        }
        let dst = claude_home.join("skills").join(skill.name());

        let dst_was_installed = dst.join("SKILL.md").exists();
        println!("Claude Code skill `{}`:", skill.name());
        skill_diff(&src, &dst, dst_was_installed)?.print();

        if dry_run {
            println!("  [dry-run] not written");
            continue;
        }
        copy_dir_replace(&src, &dst)?;
        println!("  installed in {}", dst.display());
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
    let cwd = std::env::current_dir().map_err(|e| EcpError::Output(format!("current_dir: {e}")))?;
    let path = source_skill_dir_at(skill, &cwd);
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

/// Path-resolution split out from `source_skill_dir` so unit tests can pin
/// the skill → repo-subdir mapping without touching process-global cwd
/// (`std::env::set_current_dir` races with parallel tests).
///
/// `ecp` skill: canonical source is `docs/skills/ecp/` per
/// `docs/skills/README.md` (single source-of-truth for runtime
/// `~/.claude/skills/ecp/`). Others ship from `skill_sample/claude/`.
pub(crate) fn source_skill_dir_at(skill: ClaudeSkillTarget, cwd: &std::path::Path) -> PathBuf {
    match skill {
        ClaudeSkillTarget::Ecp => cwd.join("docs").join("skills").join("ecp"),
        _ => cwd.join("skill_sample").join("claude").join(skill.name()),
    }
}

pub(crate) fn claude_skill_dir(skill: ClaudeSkillTarget) -> PathBuf {
    claude_home().join("skills").join(skill.name())
}

fn claude_home() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude")
}

impl ClaudeSkillTarget {
    pub(crate) fn name(self) -> &'static str {
        match self {
            ClaudeSkillTarget::All => "all",
            ClaudeSkillTarget::Ecp => "ecp",
            ClaudeSkillTarget::Simplify => "simplify",
        }
    }

    pub(crate) fn expand(self) -> &'static [ClaudeSkillTarget] {
        match self {
            ClaudeSkillTarget::All => &[ClaudeSkillTarget::Ecp, ClaudeSkillTarget::Simplify],
            ClaudeSkillTarget::Ecp => &[ClaudeSkillTarget::Ecp],
            ClaudeSkillTarget::Simplify => &[ClaudeSkillTarget::Simplify],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_all_expands_to_ecp_and_simplify() {
        // `All` must cover both bundled skills — previously only Simplify,
        // which left the canonical ecp skill (`docs/skills/ecp/`) out of the
        // install set and the global ~/.claude/skills/ecp went stale.
        let all = ClaudeSkillTarget::All.expand();
        assert_eq!(all.len(), 2);
        assert!(matches!(all[0], ClaudeSkillTarget::Ecp));
        assert!(matches!(all[1], ClaudeSkillTarget::Simplify));
    }

    #[test]
    fn source_skill_dir_at_ecp_points_at_docs_skills() {
        // `Ecp` sources from `docs/skills/ecp/` (canonical), not the
        // `skill_sample/claude/` codex-style sample dir. Pure path resolution —
        // no cwd manipulation so the test is parallel-safe.
        let cwd = std::path::Path::new("/fake/repo");
        let path = source_skill_dir_at(ClaudeSkillTarget::Ecp, cwd);
        assert_eq!(path, PathBuf::from("/fake/repo/docs/skills/ecp"));
    }

    #[test]
    fn source_skill_dir_at_simplify_uses_skill_sample() {
        // Other skills (Simplify) keep the legacy `skill_sample/claude/`
        // pattern — only Ecp diverges to the canonical doc path. Pure path
        // resolution — no cwd manipulation so the test is parallel-safe.
        let cwd = std::path::Path::new("/fake/repo");
        let path = source_skill_dir_at(ClaudeSkillTarget::Simplify, cwd);
        assert_eq!(
            path,
            PathBuf::from("/fake/repo/skill_sample/claude/simplify")
        );
    }

    /// Build a fake repo cwd with a source `ecp` skill so install can read it.
    fn fake_repo_with_ecp_skill(body: &str) -> tempfile::TempDir {
        let cwd = tempfile::tempdir().unwrap();
        let src = cwd.path().join("docs").join("skills").join("ecp");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), body).unwrap();
        cwd
    }

    #[test]
    fn e2e_install_skills_writes_into_claude_home() {
        let cwd = fake_repo_with_ecp_skill("v1\n");
        let home = tempfile::tempdir().unwrap();
        install_skills_at(ClaudeSkillTarget::Ecp, false, cwd.path(), home.path()).unwrap();

        let installed = home.path().join("skills").join("ecp").join("SKILL.md");
        assert!(installed.exists(), "skill must be copied into claude_home");
        assert_eq!(std::fs::read_to_string(installed).unwrap(), "v1\n");
    }

    #[test]
    fn e2e_install_dry_run_writes_nothing() {
        let cwd = fake_repo_with_ecp_skill("v1\n");
        let home = tempfile::tempdir().unwrap();
        install_skills_at(ClaudeSkillTarget::Ecp, true, cwd.path(), home.path()).unwrap();
        assert!(
            !home.path().join("skills").join("ecp").exists(),
            "dry-run must not write the skill"
        );
    }

    #[test]
    fn e2e_install_overwrites_stale_installed_copy() {
        let home = tempfile::tempdir().unwrap();
        // Pre-existing (stale) installed copy.
        let dst = home.path().join("skills").join("ecp");
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("SKILL.md"), "OLD\n").unwrap();
        // Fresh repo source differs.
        let cwd = fake_repo_with_ecp_skill("NEW\n");

        install_skills_at(ClaudeSkillTarget::Ecp, false, cwd.path(), home.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("SKILL.md")).unwrap(),
            "NEW\n",
            "install must overwrite the stale copy with repo source"
        );
    }
}
