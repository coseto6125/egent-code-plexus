//! Internal: hook-watcher detached child. Sleeps 300ms, scans reflog,
//! dispatches to rename-branch or prune.

use crate::git::safe_exec;
use clap::Args;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Args, Debug, Clone)]
pub struct HookWatcherArgs {
    #[arg(long)]
    pub branch: String,
    #[arg(long)]
    pub repo: PathBuf,
}

pub fn run(args: HookWatcherArgs) -> Result<(), gnx_core::GnxError> {
    std::thread::sleep(Duration::from_millis(300));

    let out = safe_exec::git()
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads/"])
        .current_dir(&args.repo)
        .output()?;
    if !out.status.success() {
        return Ok(());
    }
    let branches: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let pattern = format!("Branch: renamed refs/heads/{} to refs/heads/", args.branch);
    let mut rename_target: Option<String> = None;
    for cur in &branches {
        let out = safe_exec::git()
            .args(["reflog", "show", cur, "-1"])
            .current_dir(&args.repo)
            .output()?;
        if !out.status.success() {
            continue;
        }
        let log = String::from_utf8_lossy(&out.stdout);
        if log.contains(&pattern) {
            rename_target = Some(cur.clone());
            break;
        }
    }

    let gnx_bin = std::env::current_exe()?;
    let repo_arg = format!("--repo={}", args.repo.to_string_lossy());

    let mut cmd = std::process::Command::new(&gnx_bin);
    if let Some(target) = rename_target {
        let from_arg = format!("--from={}", args.branch);
        let to_arg = format!("--to={target}");
        cmd.args(["rename-branch", &from_arg, &to_arg, &repo_arg]);
    } else {
        let branch_arg = format!("--branch={}", args.branch);
        cmd.args(["prune", &branch_arg, &repo_arg]);
    }
    let _ = cmd.output();

    Ok(())
}
