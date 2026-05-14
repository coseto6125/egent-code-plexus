//! Shell-based git diff provider: invokes the user's `git` binary with `-U0`
//! and parses the unified-diff output. Matches upstream byte-for-byte.

use super::{parse_diff_hunks, DiffScope, FileDiff, GitDiffProvider};
use gnx_core::GnxError;
use std::path::Path;

pub struct ShellGitProvider;

impl GitDiffProvider for ShellGitProvider {
    fn diff(&self, repo: &Path, scope: &DiffScope) -> Result<Vec<FileDiff>, GnxError> {
        const ZERO_CONTEXT: &str = "-U0";
        let mut args: Vec<&str> = vec!["diff"];
        let base_owned: String;
        match scope {
            DiffScope::Unstaged => {}
            DiffScope::Staged => args.push("--staged"),
            DiffScope::All => args.push("HEAD"),
            DiffScope::Compare(base) => {
                base_owned = base.clone();
                args.push(&base_owned);
            }
        }
        args.push(ZERO_CONTEXT);

        let output = super::safe_exec::git()
            .args(&args)
            .current_dir(repo)
            .output()
            .map_err(|e| GnxError::GitDiff {
                reason: format!("spawn failed: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(GnxError::GitDiff {
                reason: format!(
                    "git exited with status {}: {}",
                    output.status,
                    stderr.trim()
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_diff_hunks(&stdout))
    }
}
