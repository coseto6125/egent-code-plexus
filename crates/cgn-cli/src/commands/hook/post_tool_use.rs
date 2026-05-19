//! PostToolUse handler: detect git ref-changing commands, kick off a
//! detached background reindex when the index is stale.
//!
//! Note: orphan-registry sweep used to live here, gated by a `.last-prune`
//! mtime marker on every Bash ToolUse. That introduced two problems:
//! the marker was global state shared across processes (test isolation
//! pain — see hook_post_tool_use_test flake history), and the "1-hour
//! throttle on a side-effect that runs hundreds of times per session"
//! design was over-engineered for a maintenance task that genuinely
//! needs to run once per session, not once per ToolUse. Orphan prune
//! now lives in `session_start::handle` where the trigger frequency
//! matches the work frequency — no marker, no throttle, no global
//! state.

use super::common::{
    cgn_state_dir_ensure, emit_additional_context, lookup_index_dir, strip_shell_quotes, HookInput,
};
use crate::auto_ensure::{ensure_index, EnsureResult};
use crate::background::{spawn_bg, BgJob, BgMarkers};
use cgn_core::CgnError;
use std::path::Path;
use std::sync::OnceLock;

/// Git-mutation matcher. Compiled once per process — PostToolUse fires
/// on every Bash tool call so amortising the regex build matters.
fn git_mutation_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\bgit\s+(commit|merge|rebase|cherry-pick|pull)(\s|$)").unwrap()
    })
}

pub fn handle(input: &HookInput) -> Result<(), CgnError> {
    if input.tool_name != "Bash" {
        return Ok(());
    }
    if let Some(msg) = maybe_reindex_notice(input) {
        emit_additional_context("PostToolUse", &msg);
    }
    Ok(())
}

fn maybe_reindex_notice(input: &HookInput) -> Option<String> {
    let cmd = input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !is_git_mutation(cmd) {
        return None;
    }
    let exit = input
        .tool_output
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if exit != 0 {
        return None;
    }

    // No index registered for this worktree → nothing to refresh; the
    // SessionStart hint already nagged the user to run `cgn admin index`.
    let index_dir = lookup_index_dir(&input.cwd)?;
    let repo_root = Path::new(&input.cwd);
    let graph_path = index_dir.join("graph.bin");

    let result = ensure_index(&graph_path, repo_root).unwrap_or(EnsureResult::Missing);
    let age = match result {
        EnsureResult::Stale { age_seconds } => age_seconds,
        _ => return None,
    };

    // Marker/log/lock live in the hook-local state dir so they're
    // scoped to the worktree, not shared across all worktrees that
    // happen to point at the same `~/.cgn/<repo>__<hash>/commits/<sha>/`.
    let state_dir = cgn_state_dir_ensure(&input.cwd)?;
    if !spawn_background_reindex(repo_root, &state_dir) {
        return None;
    }
    Some(format!(
        "cgn reindex started in background (index stale ~{age}s). Subsequent cgn tools may use stale data until completion (~30-120s). If it appears stuck, run `cgn admin index` manually."
    ))
}

fn is_git_mutation(cmd: &str) -> bool {
    git_mutation_re().is_match(&strip_shell_quotes(cmd))
}

/// Detached background `cgn admin index --repo <cwd>` under flock at
/// `<state_dir>/.analyze.lock`. Writes `.rebuild-complete` on success
/// or `.rebuild-failed` after MAX=3 attempts. Returns true iff the
/// launcher subprocess was spawned (the analyze outcome surfaces
/// asynchronously via marker files consumed by UserPromptSubmit).
fn spawn_background_reindex(repo_root: &Path, state_dir: &Path) -> bool {
    let repo_str = repo_root.to_string_lossy();
    let lock = state_dir.join(".analyze.lock");
    let complete = state_dir.join(".rebuild-complete");
    let failed = state_dir.join(".rebuild-failed");
    let log = state_dir.join("last-rebuild.log");

    let args: Vec<&str> = vec!["admin", "index", "--repo", repo_str.as_ref()];

    spawn_bg(BgJob {
        args: &args,
        lock: &lock,
        cwd: repo_root,
        retry: (3, 2),
        markers: Some(BgMarkers {
            log: &log,
            complete: &complete,
            failed: &failed,
        }),
    })
}

