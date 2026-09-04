//! RAII git workspace guard for `ecp diff`.
//!
//! On `enter`:
//!   1. Stash dirty tree (if any), recording whether stash was created.
//!   2. Detach HEAD to target SHA.
//!
//! On drop:
//!   3. Checkout the original ref.
//!   4. `git stash pop` if a stash was created in step 1.
//!
//! Errors during drop are logged to stderr (we cannot return from Drop).
//!
//! All git invocations go through `safe_exec::git()` per security spec §8 H4,
//! plus the filter-driver overrides below: `safe_exec` closes the paths that
//! read a repository, and these close the three here that write its worktree.

use crate::git::safe_exec;
use ecp_core::EcpError;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitGuard {
    repo_dir: PathBuf,
    original_ref: String,
    stash_created: bool,
    filter_overrides: Vec<String>,
}

/// `-c` settings that neutralise every filter driver the scanned repository
/// defines in its own `.git/config`.
///
/// `core.hooksPath=/dev/null` stops hooks and nothing else. `checkout` and
/// `stash` both convert worktree content, so a `.gitattributes` line naming
/// `filter=<driver>` runs that driver's program — reproduced here as a marker
/// file written during a checkout carrying the full `safe_exec` flag set. There
/// is no `--no-filters` to pass, and the driver name lives in the repository's
/// own attributes file, so it cannot be pre-empted blindly: the names have to
/// be read out of the config first.
///
/// All three keys need overriding. `process` takes precedence over `smudge` and
/// `clean`, so a repository that sets it keeps executing when only those two
/// are covered. `cat` is the passthrough: it hands the content back unchanged,
/// which is what an absent filter does. It is not a valid long-running filter,
/// so the `process` override makes git log an initialisation failure and fall
/// back to the content as stored — `required=false` is what keeps that a
/// fallback rather than a hard error. Those lines land in captured stderr,
/// which the success path discards.
///
/// Scope is deliberately repo-local. A driver in the user's global config is
/// the user's own — git-lfs, usually — and stays live. A driver in the scanned
/// repository's config was shipped by whoever wrote that repository.
fn repo_local_filter_overrides(repo_dir: &Path) -> Vec<String> {
    let Ok(out) = safe_exec::git()
        .args([
            "config",
            "--local",
            "--name-only",
            "--get-regexp",
            "^filter\\.",
        ])
        .current_dir(repo_dir)
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut names: Vec<&str> = text
        .lines()
        .filter_map(|key| key.strip_prefix("filter."))
        // Driver names may themselves contain dots, so split off the leaf.
        .filter_map(|rest| rest.rsplit_once('.'))
        .filter(|(_, leaf)| matches!(*leaf, "smudge" | "clean" | "process"))
        .map(|(name, _)| name)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .flat_map(|n| {
            [
                format!("filter.{n}.smudge=cat"),
                format!("filter.{n}.clean=cat"),
                format!("filter.{n}.process=cat"),
                format!("filter.{n}.required=false"),
            ]
        })
        .collect()
}

/// `safe_exec::git()` with the filter overrides applied. Every command that
/// writes the worktree goes through here.
fn git_no_filters(repo_dir: &Path, overrides: &[String]) -> Command {
    let mut cmd = safe_exec::git();
    for kv in overrides {
        cmd.arg("-c").arg(kv);
    }
    cmd.current_dir(repo_dir);
    cmd
}

impl GitGuard {
    pub fn enter(repo_dir: &Path, target_sha: &str) -> Result<Self, EcpError> {
        let filter_overrides = repo_local_filter_overrides(repo_dir);
        let original_ref = current_head_ref(repo_dir)?;
        let stash_created = stash_if_dirty(repo_dir, &filter_overrides)?;

        let out = git_no_filters(repo_dir, &filter_overrides)
            .args(["checkout", "--detach", target_sha])
            .output()
            .map_err(|e| EcpError::Output(format!("git checkout failed to spawn: {e}")))?;
        if !out.status.success() {
            // Best-effort restore stash before bailing.
            if stash_created {
                let _ = git_no_filters(repo_dir, &filter_overrides)
                    .args(["stash", "pop"])
                    .output();
            }
            return Err(EcpError::Output(format!(
                "git checkout {target_sha} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }

        Ok(GitGuard {
            repo_dir: repo_dir.to_path_buf(),
            original_ref,
            stash_created,
            filter_overrides,
        })
    }
}

impl Drop for GitGuard {
    fn drop(&mut self) {
        let restore = git_no_filters(&self.repo_dir, &self.filter_overrides)
            .args(["checkout", &self.original_ref])
            .output();
        match restore {
            Err(e) => eprintln!("GitGuard drop: git checkout failed: {e}"),
            Ok(out) if !out.status.success() => eprintln!(
                "GitGuard drop: git checkout {} stderr: {}",
                self.original_ref,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Ok(_) => {}
        }
        if self.stash_created {
            let pop = git_no_filters(&self.repo_dir, &self.filter_overrides)
                .args(["stash", "pop"])
                .output();
            if let Err(e) = pop {
                eprintln!("GitGuard drop: git stash pop failed: {e}");
            }
        }
    }
}

fn current_head_ref(repo_dir: &Path) -> Result<String, EcpError> {
    let out = safe_exec::git()
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| EcpError::Output(format!("git symbolic-ref failed: {e}")))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let out = safe_exec::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| EcpError::Output(format!("git rev-parse HEAD failed: {e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn stash_if_dirty(repo_dir: &Path, filter_overrides: &[String]) -> Result<bool, EcpError> {
    // Overridden here too: `status` compares content through the clean filter
    // whenever the stat cache cannot settle the question on its own.
    let out = git_no_filters(repo_dir, filter_overrides)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| EcpError::Output(format!("git status failed: {e}")))?;
    if out.stdout.is_empty() {
        return Ok(false);
    }
    let stash = git_no_filters(repo_dir, filter_overrides)
        .args(["stash", "push", "-u", "-m", "ecp-diff-auto-stash"])
        .output()
        .map_err(|e| EcpError::Output(format!("git stash failed: {e}")))?;
    if !stash.status.success() {
        return Err(EcpError::Output(format!(
            "git stash push failed: {}",
            String::from_utf8_lossy(&stash.stderr).trim()
        )));
    }
    Ok(true)
}
