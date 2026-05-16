//! Resolve (repo_name, branch, worktree_path) from a cwd's git state.
//! All git invocations go through safe_exec.

use crate::git::safe_exec;
use graph_nexus_core::registry::{derive_repo_name, sanitize_segment, PathError};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GitState {
    pub repo_name: String,
    // Returned for downstream registry / hook callers; only `repo_name` is read
    // by the current bin surface, the rest are covered by `tests/git_state.rs`.
    #[allow(dead_code)]
    pub branch: String,
    #[allow(dead_code)]
    pub worktree_path: PathBuf,
    #[allow(dead_code)]
    pub remote_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum GitStateError {
    #[error("not a git repository: {0}")]
    NotARepo(PathBuf),
    #[error("git command failed: {0}")]
    GitFailed(String),
    #[error("path error: {0}")]
    Path(#[from] PathError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn resolve(cwd: &Path) -> Result<GitState, GitStateError> {
    let worktree_raw = run_git(cwd, &["rev-parse", "--show-toplevel"])?;
    let worktree_path = PathBuf::from(worktree_raw.trim());
    if !worktree_path.exists() {
        return Err(GitStateError::NotARepo(cwd.to_path_buf()));
    }
    let worktree_path = worktree_path.canonicalize()?;

    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();

    let remote_url = run_git(cwd, &["remote", "get-url", "origin"])
        .ok()
        .map(|s| s.trim().to_string());

    let repo_name = match remote_url.as_deref() {
        Some(url) => derive_repo_name(Some(url))?,
        None => {
            let basename = worktree_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            // sanitize_segment rejects leading `.` or `-`; strip them first.
            let cleaned = basename.trim_start_matches(['.', '-']);
            let candidate = if cleaned.is_empty() {
                "unknown"
            } else {
                cleaned
            };
            sanitize_segment(candidate)?
        }
    };

    Ok(GitState {
        repo_name,
        branch,
        worktree_path,
        remote_url,
    })
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, GitStateError> {
    let output = safe_exec::git()
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| GitStateError::GitFailed(format!("spawn: {e}")))?;
    if !output.status.success() {
        return Err(GitStateError::GitFailed(format!(
            "git {:?} exited {}: {}",
            args,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
