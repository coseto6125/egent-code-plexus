//! `ecp uninstall` — reverse every side-effect of `ecp` setup in one shot.
//!
//! Reversal order:
//!   1. Claude Code hooks (settings.json entries)
//!   2. Claude Code MCP server (`claude mcp remove ecp`)
//!   3. Claude Code skills (~/.claude/skills/)
//!   4. Codex native patch + MCP server (~/.codex/config.toml)
//!   5. Codex skills (~/.codex/skills/)
//!   6. Gemini native skill (`gemini skills uninstall`)
//!   7. Gemini MCP server (`gemini mcp remove`)
//!   8. Git reference-transaction hook (current repo only, no --agent filter)
//!   9. ~/.ecp full wipe (unless --keep-cache or --agent is set)
//!  10. The running binary itself (unless --agent is set) — Unix unlinks it
//!      synchronously; Windows schedules a delayed delete that fires after this
//!      process exits and releases the file lock.
//!
//! Each step is resilient: a missing/uninstalled component is skipped with a
//! "skip" status entry. Errors are recorded and reported in the final summary
//! without aborting remaining steps.

use crate::commands::admin::{claude, codex, gemini};
use clap::Args;
use ecp_core::EcpError;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct UninstallArgs {
    /// Only uninstall integration for one coding agent (claude, codex, gemini).
    /// Omit to uninstall all detected agents (and remove the binary itself).
    #[arg(long)]
    pub agent: Option<String>,

    /// List what would be removed without actually deleting anything.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// Skip deletion of ~/.ecp (index cache + registry).
    #[arg(long, default_value_t = false)]
    pub keep_cache: bool,
}

pub fn run(args: UninstallArgs) -> Result<(), EcpError> {
    let agent_filter = args.agent.as_deref();
    validate_agent_filter(agent_filter)?;

    let mut summary: Vec<(&'static str, StepStatus)> = Vec::new();

    // ── Claude Code ──────────────────────────────────────────────────────────
    if matches_agent(agent_filter, "claude") {
        run_step(
            "claude-hooks",
            args.dry_run,
            remove_claude_hooks,
            &mut summary,
        );
        run_step("claude-mcp", args.dry_run, remove_claude_mcp, &mut summary);
        run_step(
            "claude-skills",
            args.dry_run,
            remove_claude_skills,
            &mut summary,
        );
    }

    // ── Codex ────────────────────────────────────────────────────────────────
    if matches_agent(agent_filter, "codex") {
        run_step(
            "codex-native",
            args.dry_run,
            remove_codex_native,
            &mut summary,
        );
        run_step("codex-mcp", args.dry_run, remove_codex_mcp, &mut summary);
        run_step(
            "codex-skills",
            args.dry_run,
            remove_codex_skills,
            &mut summary,
        );
    }

    // ── Gemini ───────────────────────────────────────────────────────────────
    if matches_agent(agent_filter, "gemini") {
        run_step(
            "gemini-native-skill",
            args.dry_run,
            remove_gemini_native,
            &mut summary,
        );
        run_step("gemini-mcp", args.dry_run, remove_gemini_mcp, &mut summary);
    }

    // ── Git hook (per-repo, only when no --agent filter) ─────────────────────
    if agent_filter.is_none() {
        run_step("git-hook", args.dry_run, remove_git_hook, &mut summary);
    }

    // ── Stop watch + background daemons before the cache wipe ────────────────
    // Otherwise the daemons race the `remove_dir_all` and re-create the dirs +
    // jsonl files we just deleted, producing ghost telemetry buckets and
    // half-restored sessions. Same gate as the cache wipe: a scoped uninstall
    // keeps daemons alive.
    if agent_filter.is_none() && !args.keep_cache {
        run_step("ecp-daemons", args.dry_run, stop_all_daemons, &mut summary);
    }

    // ── ~/.ecp wipe (only when no --agent filter and not --keep-cache) ───────
    if agent_filter.is_none() && !args.keep_cache {
        run_step("ecp-cache", args.dry_run, wipe_ecp_home, &mut summary);
    }

    // ── self binary (last; only on a full uninstall) ─────────────────────────
    // Gated like the cache wipe: a scoped `--agent` uninstall keeps `ecp` usable.
    if agent_filter.is_none() {
        run_self_binary_step(args.dry_run, &mut summary);
    }

    print_summary(&summary, args.dry_run);
    Ok(())
}

// ─── per-host removal shims ──────────────────────────────────────────────────

fn remove_claude_hooks() -> Result<(), EcpError> {
    claude::uninstall(claude::ClaudeComponent::Hooks { events: None })
}

fn remove_claude_mcp() -> Result<(), EcpError> {
    claude::uninstall(claude::ClaudeComponent::McpServer)
}

fn remove_claude_skills() -> Result<(), EcpError> {
    claude::uninstall(claude::ClaudeComponent::Skills {
        target: claude::ClaudeSkillTarget::All,
        dry_run: false,
        no_claude_md: false,
    })
}

fn remove_codex_native() -> Result<(), EcpError> {
    let path = crate::admin::host_integration::native::codex::run_uninstall()?;
    println!("codex-native: removed patch from {}", path.display());
    Ok(())
}

fn remove_codex_mcp() -> Result<(), EcpError> {
    let path = crate::admin::host_integration::mcp::codex::run_uninstall()?;
    println!("codex-mcp: removed entry from {}", path.display());
    Ok(())
}

fn remove_codex_skills() -> Result<(), EcpError> {
    codex::uninstall_skills(codex::SkillTarget::All)
}

fn remove_gemini_native() -> Result<(), EcpError> {
    gemini::uninstall(gemini::GeminiComponent::NativeSkill)
}

fn remove_gemini_mcp() -> Result<(), EcpError> {
    gemini::uninstall(gemini::GeminiComponent::McpServer)
}

// ─── git hook removal (gap A) ────────────────────────────────────────────────

fn remove_git_hook() -> Result<(), EcpError> {
    let cwd = std::env::current_dir().map_err(|e| EcpError::InvalidArgument(e.to_string()))?;
    let git_dir = match crate::git_cache::common_dir(&cwd) {
        Ok(d) => d,
        Err(e) => {
            println!("git-hook: {e}, skip");
            return Ok(());
        }
    };
    let hook_path = git_dir.join("hooks").join("reference-transaction");
    remove_git_hook_at(&hook_path)
}

/// Core of git hook removal; split so tests can exercise it with a tmpdir path
/// without running `git rev-parse`.
pub fn remove_git_hook_at(hook_path: &Path) -> Result<(), EcpError> {
    if !hook_path.exists() {
        println!("git-hook: not installed, skip");
        report_stale_backups(hook_path);
        return Ok(());
    }
    // An unreadable-but-present hook must surface, not be silently mistaken for
    // a foreign hook (which read_to_string().unwrap_or_default() would do).
    let body = std::fs::read_to_string(hook_path)
        .map_err(|e| EcpError::InvalidArgument(format!("read git hook: {e}")))?;
    if !body.contains("ecp hook-handle") && !body.contains("hook-handle") {
        println!(
            "git-hook: {} is not ecp-managed, left untouched",
            hook_path.display()
        );
        report_stale_backups(hook_path);
        return Ok(());
    }
    let chained = hook_path.with_extension("chained-prev");
    if chained.exists() {
        std::fs::rename(&chained, hook_path)?;
        println!("git-hook: restored chained hook at {}", hook_path.display());
    } else {
        std::fs::remove_file(hook_path)?;
        println!("git-hook: removed {}", hook_path.display());
    }
    report_stale_backups(hook_path);
    Ok(())
}

/// Surface `reference-transaction.bak.<timestamp>` files left by past
/// `--force` / `--no-chain` installs. NOT auto-deleted: those backups hold
/// the user's pre-ecp hook content, and silently removing them would be
/// destructive on a partial reinstall flow. Listing them is enough — the
/// user can decide whether to restore one or `rm` them.
fn report_stale_backups(hook_path: &Path) {
    let Some(parent) = hook_path.parent() else {
        return;
    };
    let Some(stem) = hook_path.file_name().and_then(|s| s.to_str()) else {
        return;
    };
    let prefix = format!("{stem}.bak.");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.starts_with(&prefix) {
            found.push(entry.path());
        }
    }
    if found.is_empty() {
        return;
    }
    found.sort();
    println!(
        "git-hook: {} pre-ecp backup(s) left in {} — review or remove manually:",
        found.len(),
        parent.display()
    );
    for path in found {
        println!("  {}", path.display());
    }
}

// ─── daemon shutdown (gap E) ─────────────────────────────────────────────────

/// SIGTERM every `watcher_pid` recorded in any
/// `~/.ecp/<repo>/sessions/<sid>/session_meta.json` so the daemons stop
/// touching the cache before `wipe_ecp_home` deletes it.
///
/// Best-effort by design — a stale pid (process already exited, or pid reused
/// by an unrelated process) is skipped via `pid_alive`. Dead-letter sessions
/// are logged then ignored.
fn stop_all_daemons() -> Result<(), EcpError> {
    let home = ecp_core::registry::resolve_home_ecp();
    if !home.exists() {
        println!("ecp-daemons: ~/.ecp does not exist, skip");
        return Ok(());
    }
    let mut sent = 0usize;
    let mut scanned = 0usize;
    for meta_path in collect_session_meta_paths(&home) {
        scanned += 1;
        if try_signal_watcher(&meta_path) {
            sent += 1;
        }
    }
    println!("ecp-daemons: signalled {sent} watcher(s) across {scanned} session(s)");
    Ok(())
}

/// Walk `~/.ecp/<repo>/sessions/*/session_meta.json`. Two `read_dir` levels —
/// shallow, no recursion past the sessions/ floor. Returns paths in arbitrary
/// order; caller doesn't care about ordering.
fn collect_session_meta_paths(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(repos) = std::fs::read_dir(home) else {
        return out;
    };
    for repo in repos.flatten() {
        let sessions_dir = repo.path().join("sessions");
        let Ok(sessions) = std::fs::read_dir(&sessions_dir) else {
            continue;
        };
        for sess in sessions.flatten() {
            let meta = sess.path().join("session_meta.json");
            if meta.is_file() {
                out.push(meta);
            }
        }
    }
    out
}

/// Send SIGTERM to the recorded watcher_pid (if alive) and zero the field so a
/// later `ecp watch --status` doesn't show a ghost. Returns `true` iff a real
/// signal was sent.
fn try_signal_watcher(meta_path: &Path) -> bool {
    let Ok(mut meta) = ecp_core::session::SessionMeta::read(meta_path) else {
        return false;
    };
    let Some(pid) = meta.watcher_pid else {
        return false;
    };
    if !ecp_core::peer::registry::pid_alive(pid) {
        // Stale pid; clear the field anyway so post-wipe state stays clean if
        // the cache wipe is later disabled with --keep-cache.
        meta.watcher_pid = None;
        let _ = ecp_core::session::SessionMeta::write_atomic(meta_path, &meta);
        return false;
    }
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
    }
    #[cfg(windows)]
    {
        // No portable SIGTERM on Windows. Best we can do without taskkill is
        // skip — wipe_ecp_home will still succeed (Windows allows unlink-while-
        // open of NTFS handles opened with FILE_SHARE_DELETE, which our
        // BufWriter does not set). Surface the gap to the user instead.
        eprintln!("  ecp-daemons: cannot signal pid {pid} on Windows — close the watcher manually");
    }
    meta.watcher_pid = None;
    let _ = ecp_core::session::SessionMeta::write_atomic(meta_path, &meta);
    true
}

// ─── ~/.ecp wipe (gap B) ─────────────────────────────────────────────────────

fn wipe_ecp_home() -> Result<(), EcpError> {
    let home = ecp_core::registry::resolve_home_ecp();
    if !home.exists() {
        println!("ecp-cache: {} does not exist, skip", home.display());
        return Ok(());
    }
    let entries = list_top_level_entries(&home)?;
    for e in &entries {
        println!("  removing {}", e.display());
    }
    std::fs::remove_dir_all(&home)
        .map_err(|e| EcpError::Output(format!("remove {}: {e}", home.display())))?;
    println!(
        "ecp-cache: removed {} ({} entries)",
        home.display(),
        entries.len()
    );
    Ok(())
}

fn list_top_level_entries(dir: &Path) -> Result<Vec<PathBuf>, EcpError> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| EcpError::Output(format!("read_dir {}: {e}", dir.display())))?;
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|de| de.path())).collect();
    entries.sort();
    Ok(entries)
}

// ─── agent filter ────────────────────────────────────────────────────────────

fn validate_agent_filter(agent: Option<&str>) -> Result<(), EcpError> {
    let Some(a) = agent else { return Ok(()) };
    match a {
        "claude" | "codex" | "gemini" => Ok(()),
        other => Err(EcpError::InvalidArgument(format!(
            "unknown agent '{other}' — expected claude, codex, or gemini"
        ))),
    }
}

fn matches_agent(filter: Option<&str>, agent: &str) -> bool {
    filter.is_none_or(|f| f == agent)
}

// ─── self binary removal (gap C) ─────────────────────────────────────────────

/// Result of attempting to remove the running binary. Distinguishes the
/// platforms because Windows cannot delete a running executable in-process: it
/// schedules a delayed delete whose success is not observable from here.
#[derive(Debug, PartialEq, Eq)]
pub enum SelfDeleteOutcome {
    /// File unlinked synchronously (Unix).
    Deleted,
    /// A delayed delete was spawned; fires after this process exits (Windows).
    Scheduled,
    /// Nothing to remove — the path did not exist.
    Skipped,
}

/// Remove `exe` — the path of the running binary. Unix unlinks it directly
/// (the inode survives until the process exits). Windows spawns a detached
/// `cmd` that waits a few seconds, by which point this process has exited and
/// released the file lock, then deletes the file. Split to take a path so a
/// test can drive it against a tmpdir, mirroring [`remove_git_hook_at`].
pub fn remove_self_binary_at(exe: &Path) -> Result<SelfDeleteOutcome, EcpError> {
    if !exe.exists() {
        return Ok(SelfDeleteOutcome::Skipped);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW: survive parent exit, no console.
        const FLAGS: u32 = 0x0000_0008 | 0x0800_0000;
        std::process::Command::new("cmd")
            .args([
                "/c",
                &format!(
                    "timeout /t 3 /nobreak >nul 2>&1 & del /f /q \"{}\"",
                    exe.display()
                ),
            ])
            .creation_flags(FLAGS)
            .spawn()
            .map_err(|e| EcpError::Output(format!("schedule self-delete: {e}")))?;
        Ok(SelfDeleteOutcome::Scheduled)
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(exe)
            .map_err(|e| EcpError::Output(format!("remove {}: {e}", exe.display())))?;
        Ok(SelfDeleteOutcome::Deleted)
    }
}

fn run_self_binary_step(dry_run: bool, summary: &mut Vec<(&'static str, StepStatus)>) {
    const LABEL: &str = "self-binary";
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("current_exe: {e}");
            eprintln!("  {LABEL}: error — {msg}");
            summary.push((LABEL, StepStatus::Failed(msg)));
            return;
        }
    };
    if dry_run {
        println!("[dry-run] would remove binary: {}", exe.display());
        summary.push((LABEL, StepStatus::Skipped("dry-run".into())));
        return;
    }
    match remove_self_binary_at(&exe) {
        Ok(SelfDeleteOutcome::Deleted) => {
            println!("self-binary: removed {}", exe.display());
            summary.push((LABEL, StepStatus::Done));
        }
        Ok(SelfDeleteOutcome::Scheduled) => {
            println!("self-binary: scheduled delete of {}", exe.display());
            summary.push((LABEL, StepStatus::Scheduled));
        }
        Ok(SelfDeleteOutcome::Skipped) => {
            summary.push((LABEL, StepStatus::Skipped("binary not found".into())));
        }
        Err(e) => {
            let msg = e.to_string();
            eprintln!("  {LABEL}: error — {msg}");
            summary.push((LABEL, StepStatus::Failed(msg)));
        }
    }
}

// ─── step runner ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum StepStatus {
    Done,
    /// Action spawned but not yet completed (Windows delayed self-delete).
    Scheduled,
    Skipped(String),
    Failed(String),
}

fn run_step<F>(
    label: &'static str,
    dry_run: bool,
    f: F,
    summary: &mut Vec<(&'static str, StepStatus)>,
) where
    F: FnOnce() -> Result<(), EcpError>,
{
    if dry_run {
        println!("[dry-run] would remove: {label}");
        summary.push((label, StepStatus::Skipped("dry-run".into())));
        return;
    }
    match f() {
        Ok(()) => summary.push((label, StepStatus::Done)),
        Err(e) => {
            let msg = e.to_string();
            if is_not_installed(&msg) {
                summary.push((label, StepStatus::Skipped(msg)));
            } else {
                eprintln!("  {label}: error — {msg}");
                summary.push((label, StepStatus::Failed(msg)));
            }
        }
    }
}

/// Errors that indicate the component was never installed.
/// Treated as graceful skips so a partial install still completes cleanly.
fn is_not_installed(msg: &str) -> bool {
    let low = msg.to_lowercase();
    low.contains("not found")
        || low.contains("no such file")
        || low.contains("does not exist")
        || low.contains("not installed")
        || low.contains("already removed")
        || low.contains("was not found")
}

fn print_summary(summary: &[(&'static str, StepStatus)], dry_run: bool) {
    if dry_run {
        println!("\n[dry-run] no changes made.");
        return;
    }
    println!("\nuninstall summary:");
    for (label, status) in summary {
        match status {
            StepStatus::Done => println!("  {label:<24} done"),
            StepStatus::Scheduled => {
                println!("  {label:<24} scheduled (deletes after exit)")
            }
            StepStatus::Skipped(reason) => println!("  {label:<24} skip  ({reason})"),
            StepStatus::Failed(reason) => println!("  {label:<24} ERROR ({reason})"),
        }
    }
}
