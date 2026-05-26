//! Scriptable Claude Code host integration commands for AI agents.

use crate::admin::host_integration::mcp::claude_code as mcp_claude;
use crate::commands::admin::claude_code as hooks;
use crate::commands::admin::skill_fs::{copy_dir_replace, skill_diff};
use crate::commands::admin::skill_source::{resolve, EmbeddedTree, SkillSource};
use clap::Subcommand;
use ecp_core::EcpError;
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
    /// Skill packs that teach Claude when to use ecp beyond command help.
    Skills {
        /// Which skill(s) to install. Defaults to all.
        #[arg(value_enum, default_value_t = ClaudeSkillTarget::All)]
        target: ClaudeSkillTarget,
        /// Print the diff against the installed copy without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip injecting @ECP.md guidance import into ~/.claude/CLAUDE.md.
        #[arg(long)]
        no_claude_md: bool,
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
        ClaudeComponent::Skills {
            target,
            dry_run,
            no_claude_md,
        } => install_skills(target, dry_run, no_claude_md),
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
        ClaudeComponent::Skills { target, .. } => {
            uninstall_skills(target)?;
            uninstall_ecp_import_at(&claude_home())
        }
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

pub(crate) fn install_skills(
    target: ClaudeSkillTarget,
    dry_run: bool,
    no_claude_md: bool,
) -> Result<(), EcpError> {
    let cwd = std::env::current_dir().map_err(|e| EcpError::Output(format!("current_dir: {e}")))?;
    let claude_home = claude_home();
    install_skills_at(target, dry_run, &cwd, &claude_home)?;
    if !no_claude_md {
        // ECP.md ships inside the `ecp` skill, so resolve that skill's source
        // (repo tree or embedded copy) and read ECP.md from it.
        let ecp_src = source_skill_dir_at(ClaudeSkillTarget::Ecp, &cwd)?;
        let ecp_md_src = ecp_src.path().join("ECP.md");
        inject_ecp_import_at(&claude_home, &ecp_md_src, dry_run)?;
    }
    Ok(())
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
        let src = source_skill_dir_at(skill, cwd)?;
        let src = src.path();
        let dst = claude_home.join("skills").join(skill.name());

        let dst_was_installed = dst.join("SKILL.md").exists();
        println!("Claude Code skill `{}`:", skill.name());
        skill_diff(src, &dst, dst_was_installed)?.print();

        if dry_run {
            println!("  [dry-run] not written");
            continue;
        }
        copy_dir_replace(src, &dst)?;
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

/// Copy `ecp_md_src` to `<claude_home>/ECP.md` and append `@ECP.md` to
/// `<claude_home>/CLAUDE.md` if the line is not already present.
///
/// Explicit `claude_home` parameter makes this testable without mutating
/// process-global HOME (matches the `install_skills_at` pattern).
pub(crate) fn inject_ecp_import_at(
    claude_home: &Path,
    ecp_md_src: &Path,
    dry_run: bool,
) -> Result<(), EcpError> {
    let dest_ecp_md = claude_home.join("ECP.md");
    let dest_claude_md = claude_home.join("CLAUDE.md");

    if dry_run {
        println!(
            "  [dry-run] would copy {} → {}",
            ecp_md_src.display(),
            dest_ecp_md.display()
        );
        println!(
            "  [dry-run] would add @ECP.md import to {}",
            dest_claude_md.display()
        );
        return Ok(());
    }

    // Managed file — always overwrite so it tracks the bundled source.
    fs::create_dir_all(claude_home)?;
    fs::copy(ecp_md_src, &dest_ecp_md).map_err(|e| {
        EcpError::Output(format!(
            "cannot copy ECP.md source {}: {e}",
            ecp_md_src.display()
        ))
    })?;

    // Idempotent: only append when the import line is absent.
    let existing = if dest_claude_md.exists() {
        fs::read_to_string(&dest_claude_md)?
    } else {
        String::new()
    };
    let already_present = existing.lines().any(|l| l.trim() == "@ECP.md");
    if !already_present {
        let separator = if existing.ends_with('\n') || existing.is_empty() {
            ""
        } else {
            "\n"
        };
        let updated = format!("{existing}{separator}@ECP.md\n");
        fs::write(&dest_claude_md, updated)?;
        println!("  added @ECP.md import to {}", dest_claude_md.display());
    } else {
        println!(
            "  @ECP.md already present in {} — skipped",
            dest_claude_md.display()
        );
    }
    Ok(())
}

/// Remove the `@ECP.md` line from `<claude_home>/CLAUDE.md` and delete
/// `<claude_home>/ECP.md`. Both steps are idempotent.
pub(crate) fn uninstall_ecp_import_at(claude_home: &Path) -> Result<(), EcpError> {
    let dest_ecp_md = claude_home.join("ECP.md");
    let dest_claude_md = claude_home.join("CLAUDE.md");

    if dest_ecp_md.exists() {
        fs::remove_file(&dest_ecp_md)?;
        println!("  removed {}", dest_ecp_md.display());
    }

    if dest_claude_md.exists() {
        let existing = fs::read_to_string(&dest_claude_md)?;
        // Match install's exact insertion (`@ECP.md\n`, optionally preceded by a
        // separator newline) and splice it out byte-for-byte. Rebuilding via
        // `.lines()` would normalize CRLF→LF and add a trailing newline, mutating
        // the user's file even when no import line is present.
        let stripped = existing
            .replace("\n@ECP.md\n", "\n")
            .replace("@ECP.md\n", "")
            .replace("\n@ECP.md", "");
        if stripped != existing {
            fs::write(&dest_claude_md, &stripped)?;
            println!("  removed @ECP.md import from {}", dest_claude_md.display());
        }
    }
    Ok(())
}

/// Path-resolution split out from `source_skill_dir` so unit tests can pin
/// the skill → repo-subdir mapping without touching process-global cwd
/// (`std::env::set_current_dir` races with parallel tests).
///
/// `ecp` skill: canonical source is `docs/skills/ecp/` per
/// `docs/skills/README.md` (single source-of-truth for runtime
/// `~/.claude/skills/ecp/`). Others ship from `skill_sample/claude/`.
///
/// Both are embedded in the binary, so a package-manager install (no repo tree
/// under `cwd`) still resolves a usable source — see [`skill_source`].
pub(crate) fn source_skill_dir_at(
    skill: ClaudeSkillTarget,
    cwd: &std::path::Path,
) -> Result<SkillSource, EcpError> {
    match skill {
        ClaudeSkillTarget::Ecp => resolve(EmbeddedTree::EcpSkill, cwd),
        _ => resolve(EmbeddedTree::ClaudeSample(skill.name()), cwd),
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
    fn source_skill_dir_at_ecp_prefers_repo_docs_skills() {
        // When `cwd` holds the repo tree, `Ecp` sources from `docs/skills/ecp/`
        // (canonical) — verbatim, not a materialized embedded copy.
        let cwd = fake_repo_with_ecp_skill("v1\n");
        let src = source_skill_dir_at(ClaudeSkillTarget::Ecp, cwd.path()).unwrap();
        assert_eq!(src.path(), cwd.path().join("docs/skills/ecp"));
    }

    #[test]
    fn source_skill_dir_at_ecp_falls_back_to_embedded() {
        // No repo tree under `cwd` (the npx/uvx case) → embedded copy resolves
        // anyway, so the skill is installable without a checkout.
        let cwd = tempfile::tempdir().unwrap();
        let src = source_skill_dir_at(ClaudeSkillTarget::Ecp, cwd.path()).unwrap();
        assert!(src.path().join("SKILL.md").is_file());
        assert_ne!(src.path(), cwd.path().join("docs/skills/ecp"));
    }

    #[test]
    fn source_skill_dir_at_simplify_falls_back_to_embedded() {
        // Simplify (skill_sample/claude/) is embedded too — installable with no
        // repo tree under `cwd`.
        let cwd = tempfile::tempdir().unwrap();
        let src = source_skill_dir_at(ClaudeSkillTarget::Simplify, cwd.path()).unwrap();
        assert!(src.path().join("SKILL.md").is_file());
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

    /// Build a fake repo with both SKILL.md and ECP.md under docs/skills/ecp/.
    fn fake_repo_with_ecp_guidance(guidance: &str) -> tempfile::TempDir {
        let cwd = tempfile::tempdir().unwrap();
        let src = cwd.path().join("docs").join("skills").join("ecp");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "skill\n").unwrap();
        std::fs::write(src.join("ECP.md"), guidance).unwrap();
        cwd
    }

    #[test]
    fn e2e_inject_creates_claude_md_with_import() {
        // No pre-existing CLAUDE.md → inject must create it with @ECP.md.
        let home = tempfile::tempdir().unwrap();
        let cwd = fake_repo_with_ecp_guidance("guidance\n");
        let src = cwd
            .path()
            .join("docs")
            .join("skills")
            .join("ecp")
            .join("ECP.md");

        inject_ecp_import_at(home.path(), &src, false).unwrap();

        let claude_md = home.path().join("CLAUDE.md");
        assert!(claude_md.exists(), "CLAUDE.md must be created");
        let content = std::fs::read_to_string(&claude_md).unwrap();
        assert!(
            content.lines().any(|l| l.trim() == "@ECP.md"),
            "CLAUDE.md must contain @ECP.md"
        );
        assert!(home.path().join("ECP.md").exists(), "ECP.md must be copied");
    }

    #[test]
    fn e2e_inject_is_idempotent() {
        // Running inject twice must not duplicate the @ECP.md line.
        let home = tempfile::tempdir().unwrap();
        let cwd = fake_repo_with_ecp_guidance("guidance\n");
        let src = cwd
            .path()
            .join("docs")
            .join("skills")
            .join("ecp")
            .join("ECP.md");

        inject_ecp_import_at(home.path(), &src, false).unwrap();
        inject_ecp_import_at(home.path(), &src, false).unwrap();

        let content = std::fs::read_to_string(home.path().join("CLAUDE.md")).unwrap();
        let count = content.lines().filter(|l| l.trim() == "@ECP.md").count();
        assert_eq!(count, 1, "@ECP.md must appear exactly once");
    }

    #[test]
    fn e2e_inject_appends_to_existing_claude_md() {
        // Existing CLAUDE.md content must be preserved; @ECP.md appended at EOF.
        let home = tempfile::tempdir().unwrap();
        let prior = "# My rules\ndo stuff\n";
        std::fs::write(home.path().join("CLAUDE.md"), prior).unwrap();

        let cwd = fake_repo_with_ecp_guidance("guidance\n");
        let src = cwd
            .path()
            .join("docs")
            .join("skills")
            .join("ecp")
            .join("ECP.md");
        inject_ecp_import_at(home.path(), &src, false).unwrap();

        let content = std::fs::read_to_string(home.path().join("CLAUDE.md")).unwrap();
        assert!(
            content.starts_with("# My rules"),
            "prior content must be preserved"
        );
        assert!(content.contains("@ECP.md"), "@ECP.md must be appended");
    }

    #[test]
    fn e2e_inject_dry_run_writes_nothing() {
        // dry_run=true must leave the claude_home completely untouched.
        let home = tempfile::tempdir().unwrap();
        let cwd = fake_repo_with_ecp_guidance("guidance\n");
        let src = cwd
            .path()
            .join("docs")
            .join("skills")
            .join("ecp")
            .join("ECP.md");

        inject_ecp_import_at(home.path(), &src, true).unwrap();

        assert!(
            !home.path().join("ECP.md").exists(),
            "dry-run must not write ECP.md"
        );
        assert!(
            !home.path().join("CLAUDE.md").exists(),
            "dry-run must not write CLAUDE.md"
        );
    }

    #[test]
    fn e2e_uninstall_removes_import_and_file() {
        // After inject, uninstall must strip @ECP.md and delete ECP.md; other
        // CLAUDE.md content must survive.
        let home = tempfile::tempdir().unwrap();
        let prior = "# My rules\ndo stuff\n";
        std::fs::write(home.path().join("CLAUDE.md"), prior).unwrap();

        let cwd = fake_repo_with_ecp_guidance("guidance\n");
        let src = cwd
            .path()
            .join("docs")
            .join("skills")
            .join("ecp")
            .join("ECP.md");
        inject_ecp_import_at(home.path(), &src, false).unwrap();

        uninstall_ecp_import_at(home.path()).unwrap();

        assert!(
            !home.path().join("ECP.md").exists(),
            "uninstall must remove ECP.md"
        );
        let content = std::fs::read_to_string(home.path().join("CLAUDE.md")).unwrap();
        assert!(
            !content.lines().any(|l| l.trim() == "@ECP.md"),
            "@ECP.md must be removed from CLAUDE.md"
        );
        assert!(
            content.contains("# My rules"),
            "prior CLAUDE.md content must survive"
        );
    }

    #[test]
    fn e2e_uninstall_leaves_unrelated_claude_md_byte_identical() {
        // Regression: uninstall must not touch a CLAUDE.md that has no @ECP.md
        // import — even one without a trailing newline (lines()+rejoin would add
        // one and rewrite the file unconditionally).
        let home = tempfile::tempdir().unwrap();
        let prior = "# My rules\r\nno trailing newline";
        std::fs::write(home.path().join("CLAUDE.md"), prior).unwrap();

        uninstall_ecp_import_at(home.path()).unwrap();

        let content = std::fs::read_to_string(home.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, prior, "unrelated CLAUDE.md must be byte-identical");
    }

    #[test]
    fn e2e_uninstall_preserves_no_trailing_newline_when_stripping_import() {
        // Regression: stripping @ECP.md must splice only that line, preserving the
        // surrounding bytes (no CRLF→LF, no added trailing newline).
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("CLAUDE.md"),
            "# rules\n@ECP.md\nlast no nl",
        )
        .unwrap();

        uninstall_ecp_import_at(home.path()).unwrap();

        let content = std::fs::read_to_string(home.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, "# rules\nlast no nl");
    }
}
