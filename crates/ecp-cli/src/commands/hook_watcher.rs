//! Internal: detached child spawned by `hook-handle` on a branch delete.
//!
//! The index is addressed by commit, not by branch, so a branch delete or
//! rename leaves nothing to prune or rename. This used to shell out to
//! `rename-branch` and `prune`, neither of which exists as a top-level
//! command; the failure was swallowed. The command stays only so already
//! installed `reference-transaction` hooks that spawn it keep exiting 0.

use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct HookWatcherArgs {
    #[arg(long)]
    pub branch: String,
    #[arg(long)]
    pub repo: PathBuf,
}

pub fn run(_args: HookWatcherArgs) -> Result<(), ecp_core::EcpError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn test_run_deleted_branch_in_missing_repo_returns_ok_without_work() {
        let started = Instant::now();
        let args = HookWatcherArgs {
            branch: "gone".into(),
            repo: PathBuf::from("/nonexistent/ecp-hook-watcher-test"),
        };
        run(args).expect("a branch delete leaves nothing to do, so it cannot fail");
        assert!(started.elapsed() < Duration::from_millis(250));
    }
}
