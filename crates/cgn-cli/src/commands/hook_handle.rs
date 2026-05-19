//! `cgn hook-handle <stage>`: invoked by reference-transaction hook.
//! On `committed`, parses stdin for branch DELETE events and spawns
//! detached watchers via graph-nexus-core daemon helper.

use clap::Args;
use std::io::BufRead;

#[derive(Args, Debug, Clone)]
pub struct HookHandleArgs {
    /// Reference-transaction stage: prepared / committed / aborted.
    pub stage: String,
}

const ALL_ZERO: &str = "0000000000000000000000000000000000000000";

pub fn run(args: HookHandleArgs) -> Result<(), cgn_core::CgnError> {
    if args.stage != "committed" {
        return Ok(());
    }

    let stdin = std::io::stdin();
    let deleted: Vec<String> = stdin
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() != 3 {
                return None;
            }
            let (_old, new, refname) = (parts[0], parts[1], parts[2]);
            if new == ALL_ZERO && refname.starts_with("refs/heads/") {
                Some(refname.trim_start_matches("refs/heads/").to_string())
            } else {
                None
            }
        })
        .collect();

    if deleted.is_empty() {
        return Ok(());
    }

    let repo = std::env::current_dir()?;
    let repo_str = repo.to_string_lossy().into_owned();
    let cgn_bin = std::env::current_exe()?;
    let cgn_bin_str = cgn_bin.to_string_lossy().into_owned();

    for branch in deleted {
        let branch_arg = format!("--branch={}", branch);
        let repo_arg = format!("--repo={}", repo_str);
        let args_vec: Vec<&str> = vec![
            cgn_bin_str.as_str(),
            "hook-watcher",
            branch_arg.as_str(),
            repo_arg.as_str(),
        ];
        let _ = cgn_core::daemon::spawn_detached(&args_vec);
    }

    Ok(())
}
