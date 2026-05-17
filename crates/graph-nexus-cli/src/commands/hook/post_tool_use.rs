//! PostToolUse handler: detect git ref-changing commands, kick off a
//! detached background reindex when the index is stale.

use super::common::{
    emit_additional_context, gnx_state_dir_ensure, lookup_index_dir, strip_shell_quotes, HookInput,
};
use crate::auto_ensure::{ensure_index, EnsureResult};
use crate::background::{spawn_bg, BgJob, BgMarkers};
use graph_nexus_core::GnxError;
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

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    if input.tool_name != "Bash" {
        return Ok(());
    }
    // Both signals (orphan-prune notice + reindex notice) merge into one
    // additionalContext payload — Claude Code parses one JSON object on
    // stdout, so two println!s would drop the second silently.
    let mut sections: Vec<String> = Vec::new();

    // Orphan-registry sweep is registry-level; not gated on git/index
    // state so it actually fires on idle Edit-only sessions too. The
    // 1-hour throttle keeps the per-call stat negligible.
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    if should_run_orphan_prune(&home_gnx) && spawn_background_prune(&home_gnx) {
        sections.push(
            "gnx orphan-registry sweep started in background. Stale ~/.gnx/<repo>__<hash>/ entries from deleted worktrees will be cleaned. Failures (if any) surface via UserPromptSubmit.".to_string(),
        );
    }

    if let Some(msg) = maybe_reindex_notice(input) {
        sections.push(msg);
    }
    if !sections.is_empty() {
        emit_additional_context("PostToolUse", &sections.join("\n\n"));
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
    // SessionStart hint already nagged the user to run `gnx admin index`.
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
    // happen to point at the same `~/.gnx/<repo>__<hash>/commits/<sha>/`.
    let state_dir = gnx_state_dir_ensure(&input.cwd)?;
    if !spawn_background_reindex(repo_root, &state_dir) {
        return None;
    }
    Some(format!(
        "gnx reindex started in background (index stale ~{age}s). Subsequent gnx tools may use stale data until completion (~30-120s). If it appears stuck, run `gnx admin index` manually."
    ))
}

fn is_git_mutation(cmd: &str) -> bool {
    git_mutation_re().is_match(&strip_shell_quotes(cmd))
}

/// Detached background `gnx admin index --repo <cwd>` under flock at
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

/// Detached background `gnx admin prune --orphans` under flock at
/// `<home_gnx>/.prune.lock`. Writes `.prune-complete` on success or
/// `.prune-failed` on failure. Returns true iff the launcher spawned.
fn spawn_background_prune(home_gnx: &Path) -> bool {
    let lock = home_gnx.join(".prune.lock");
    let complete = home_gnx.join(".prune-complete");
    let failed = home_gnx.join(".prune-failed");
    let log = home_gnx.join("last-prune.log");

    spawn_bg(BgJob {
        args: &["admin", "prune", "--orphans"],
        lock: &lock,
        cwd: home_gnx,
        retry: (1, 0),
        markers: Some(BgMarkers {
            log: &log,
            complete: &complete,
            failed: &failed,
        }),
    })
}

/// Check if orphan prune should run (at most once per hour).
/// Updates the throttle marker before returning true.
fn should_run_orphan_prune(home_gnx: &Path) -> bool {
    let marker = home_gnx.join(".last-prune");
    let now = std::time::SystemTime::now();
    let due = match std::fs::metadata(&marker).and_then(|m| m.modified()) {
        Ok(mtime) => now
            .duration_since(mtime)
            .map(|d| d.as_secs() >= 3600)
            .unwrap_or(true),
        Err(_) => true, // no marker = first run, fire
    };
    if due {
        // Touch the marker BEFORE the spawn so concurrent invocations no-op.
        let _ = std::fs::write(&marker, b"");
    }
    due
}
