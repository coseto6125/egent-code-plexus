//! Resolve a `--baseline <ref>` value to a concrete commit SHA.
//!
//! Accepted forms:
//! - Branch:       `main`, `origin/main`
//! - Tag:          `v1.2.0`
//! - Commit SHA:   `a8b2f54` (short or full)
//! - Relative:     `HEAD~5`
//! - PR number:    `PR/13` (requires `gh` CLI authenticated to the repo)

use crate::git::safe_exec;
use cgn_core::GnxError;
use std::path::Path;
use std::process::Command;

/// Resolve `ref_str` to a 40-char commit SHA inside the given repo dir.
pub fn resolve(ref_str: &str, repo_dir: &Path) -> Result<String, GnxError> {
    if let Some(pr_num) = ref_str.strip_prefix("PR/") {
        return resolve_pr(pr_num, repo_dir);
    }
    let sha = resolve_via_git(ref_str, repo_dir)?;
    warn_if_local_diverges_from_remote(ref_str, &sha, repo_dir);
    Ok(sha)
}

/// When `ref_str` is a short branch name (no `/`), check whether the local
/// ref differs from `origin/<ref_str>` and warn on divergence. Silent when:
///   - ref contains `/` (already qualified)
///   - `origin/<ref_str>` doesn't exist (local-only branch)
///   - SHAs match
fn warn_if_local_diverges_from_remote(ref_str: &str, local_sha: &str, repo_dir: &Path) {
    if ref_str.contains('/') {
        return;
    }
    let remote_ref = format!("origin/{ref_str}");
    let Ok(remote_sha) = resolve_via_git(&remote_ref, repo_dir) else {
        eprintln!("note: `{remote_ref}` not configured; baseline divergence check skipped.");
        return;
    };
    if remote_sha == local_sha {
        return;
    }
    eprintln!(
        "warning: local `{ref_str}` ({}) differs from `{remote_ref}` ({}).\n\
         cgn is using local. Sync with `git pull --ff-only origin {ref_str}`,\n\
         or pass `--baseline {remote_ref}` explicitly.",
        &local_sha[..7.min(local_sha.len())],
        &remote_sha[..7.min(remote_sha.len())]
    );
}

fn resolve_via_git(ref_str: &str, repo_dir: &Path) -> Result<String, GnxError> {
    let out = safe_exec::git()
        .args(["rev-parse", "--verify", &format!("{ref_str}^{{commit}}")])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| GnxError::Output(format!("git rev-parse failed to spawn: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(GnxError::Output(format!(
            "cannot resolve baseline `{ref_str}`: {}\n\
             accepted: branch / tag / commit SHA / HEAD~N / PR/<n>",
            stderr.trim()
        )));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.len() < 7 {
        return Err(GnxError::Output(format!(
            "git rev-parse returned suspect output for `{ref_str}`: `{sha}`"
        )));
    }
    Ok(sha)
}

fn resolve_pr(pr_num: &str, repo_dir: &Path) -> Result<String, GnxError> {
    if !pr_num.chars().all(|c| c.is_ascii_digit()) {
        return Err(GnxError::Output(format!(
            "PR number must be numeric, got `{pr_num}`"
        )));
    }
    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            pr_num,
            "--json",
            "baseRefOid",
            "--jq",
            ".baseRefOid",
        ])
        .current_dir(repo_dir)
        .output()
        .map_err(|_| {
            GnxError::Output("gh CLI not found; install gh or pass commit SHA directly".into())
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(GnxError::Output(format!(
            "cannot resolve PR/{pr_num}: {}",
            stderr.trim()
        )));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(GnxError::Output(format!(
            "gh pr view {pr_num} returned empty baseRefOid (PR/{pr_num})"
        )));
    }
    Ok(sha)
}
