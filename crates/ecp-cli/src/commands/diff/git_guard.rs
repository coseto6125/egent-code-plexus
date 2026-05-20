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

use crate::git::safe_exec;
use ecp_core::EcpError;
use std::path::{Path, PathBuf};

pub struct GitGuard {
    repo_dir: PathBuf,
    original_ref: String,
    stash_created: bool,
}

impl GitGuard {
    pub fn enter(repo_dir: &Path, target_sha: &str) -> Result<Self, EcpError> {
        let original_ref = current_head_ref(repo_dir)?;
        let stash_created = stash_if_dirty(repo_dir)?;

        let out = safe_exec::git()
            .args(["checkout", "--detach", target_sha])
            .current_dir(repo_dir)
            .output()
            .map_err(|e| EcpError::Output(format!("git checkout failed to spawn: {e}")))?;
        if !out.status.success() {
            // Best-effort restore stash before bailing.
            if stash_created {
                let _ = safe_exec::git()
                    .args(["stash", "pop"])
                    .current_dir(repo_dir)
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
        })
    }
}

impl Drop for GitGuard {
    fn drop(&mut self) {
        let restore = safe_exec::git()
            .args(["checkout", &self.original_ref])
            .current_dir(&self.repo_dir)
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
            let pop = safe_exec::git()
                .args(["stash", "pop"])
                .current_dir(&self.repo_dir)
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

fn stash_if_dirty(repo_dir: &Path) -> Result<bool, EcpError> {
    let out = safe_exec::git()
        .args(["status", "--porcelain"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| EcpError::Output(format!("git status failed: {e}")))?;
    if out.stdout.is_empty() {
        return Ok(false);
    }
    let stash = safe_exec::git()
        .args(["stash", "push", "-u", "-m", "ecp-diff-auto-stash"])
        .current_dir(repo_dir)
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
