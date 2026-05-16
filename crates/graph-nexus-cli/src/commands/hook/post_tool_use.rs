//! PostToolUse handler: detect git ref-changing commands, kick off a
//! detached background reindex when the index is stale.

use super::common::{
    emit_additional_context, gnx_state_dir_ensure, lookup_index_dir, strip_shell_quotes, HookInput,
};
use crate::auto_ensure::{ensure_index, EnsureResult};
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
    let cmd = input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !is_git_mutation(cmd) {
        return Ok(());
    }
    let exit = input
        .tool_output
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if exit != 0 {
        return Ok(());
    }

    // No index registered for this worktree → nothing to refresh; the
    // SessionStart hint already nagged the user to run `gnx admin index`.
    let index_dir = match lookup_index_dir(&input.cwd) {
        Some(d) => d,
        None => return Ok(()),
    };
    let repo_root = Path::new(&input.cwd);
    let graph_path = index_dir.join("graph.bin");

    let result = ensure_index(&graph_path, repo_root).unwrap_or(EnsureResult::Missing);
    let age = match result {
        EnsureResult::Stale { age_seconds } => age_seconds,
        _ => return Ok(()),
    };

    // Marker/log/lock live in the hook-local state dir so they're
    // scoped to the worktree, not shared across all branches that
    // happen to share a `~/.gnx/<repo>/<branch>/` directory.
    let state_dir = match gnx_state_dir_ensure(&input.cwd) {
        Some(d) => d,
        None => return Ok(()),
    };

    if !spawn_background_reindex(repo_root, &state_dir) {
        return Ok(());
    }

    emit_additional_context(
        "PostToolUse",
        &format!(
            "gnx reindex started in background (index stale ~{age}s). Subsequent gnx tools may use stale data until completion (~30-120s). If it appears stuck, run `gnx admin index` manually."
        ),
    );
    Ok(())
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

    crate::background::spawn_bg(crate::background::BgJob {
        args: &["admin", "index", "--repo", repo_str.as_ref()],
        lock: &lock,
        cwd: repo_root,
        retry: (3, 2),
        markers: Some(crate::background::BgMarkers {
            log: &log,
            complete: &complete,
            failed: &failed,
        }),
    })
}
