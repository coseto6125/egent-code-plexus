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
//! All git invocations go through `safe_exec::git()` per security spec §8 H4.
//! The three that write the worktree — `stash push`, `checkout`, `stash pop` —
//! additionally carry `safe_exec::repo_local_filter_overrides`, because content
//! conversion is what runs a repository's own filter driver.

use crate::git::safe_exec;
use ecp_core::EcpError;
use std::path::{Path, PathBuf};

pub struct GitGuard {
    repo_dir: PathBuf,
    original_ref: String,
    stash_created: bool,
    filter_overrides: Vec<String>,
}

impl GitGuard {
    pub fn enter(repo_dir: &Path, target_sha: &str) -> Result<Self, EcpError> {
        let filter_overrides = safe_exec::repo_local_filter_overrides(repo_dir);
        let original_ref = current_head_ref(repo_dir)?;
        let stash_created = stash_if_dirty(repo_dir, &filter_overrides)?;

        let out = safe_exec::git_with_overrides(repo_dir, &filter_overrides)
            .args(["checkout", "--detach", target_sha])
            .output()
            .map_err(|e| EcpError::Output(format!("git checkout failed to spawn: {e}")))?;
        if !out.status.success() {
            // Best-effort restore stash before bailing.
            if stash_created {
                let _ = safe_exec::git_with_overrides(repo_dir, &filter_overrides)
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
        let restore = safe_exec::git_with_overrides(&self.repo_dir, &self.filter_overrides)
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
            let pop = safe_exec::git_with_overrides(&self.repo_dir, &self.filter_overrides)
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
    let out = safe_exec::git_with_overrides(repo_dir, filter_overrides)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| EcpError::Output(format!("git status failed: {e}")))?;
    if out.stdout.is_empty() {
        return Ok(false);
    }
    let stash = safe_exec::git_with_overrides(repo_dir, filter_overrides)
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
