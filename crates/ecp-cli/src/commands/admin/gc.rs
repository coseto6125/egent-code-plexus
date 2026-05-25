use clap::Args;

/// `ecp admin gc` — converge stale graph generations + sweep retired repo/session
/// dirs across all repos under `~/.ecp`. Idempotent; safe to run repeatedly.
#[derive(Args, Debug, Clone)]
pub struct GcArgs {
    /// Print what would be removed without deleting.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: GcArgs) -> Result<(), ecp_core::EcpError> {
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let mut total_removed = 0usize;

    // L1: top-level retired repo dirs.
    if !args.dry_run {
        match crate::admin::gc::sweep_retired_repos(&home_ecp) {
            Ok(s) => total_removed += s.removed,
            Err(e) => eprintln!("gc: sweep_retired_repos: {e}"),
        }
    }

    // L2 + L3: per-repo generation convergence + session sweep.
    if let Ok(it) = std::fs::read_dir(&home_ecp) {
        for entry in it.flatten() {
            let repo_root = entry.path();
            if !repo_root.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.contains(".dead") || name == "telemetry" {
                continue;
            }
            if args.dry_run {
                continue;
            }
            if let Ok(s) = crate::admin::gc::sweep_stale_generations(&repo_root) {
                total_removed += s.removed;
            }
            if let Ok(s) = crate::admin::gc::sweep_sessions(&repo_root) {
                total_removed += s.removed;
            }
        }
    }

    println!("gc: removed {total_removed} stale/retired dirs");
    Ok(())
}
