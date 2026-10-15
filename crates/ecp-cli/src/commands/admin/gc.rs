use clap::Args;

/// `ecp admin gc` — converge stale graph generations + sweep retired repo/session
/// dirs across all repos under `~/.ecp`. Idempotent; safe to run repeatedly.
#[derive(Args, Debug, Clone)]
pub struct GcArgs {
    /// Skip all deletions (no-op run). Does not yet enumerate candidates.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip when the last completed gc finished less than 12 h ago. For
    /// automatic triggers; an upgrade or a manual run omits it.
    #[arg(long)]
    pub auto: bool,
}

/// The sweeps judge on a 7-day grace (24 h for sessions), so an automatic
/// run more often than this finds nothing new; each run costs two git calls
/// per indexed repo (~3 s with ~120 repos, measured 2026-10-15).
const AUTO_GC_INTERVAL_SECS: u64 = 12 * 3600;
const LAST_GC_STAMP: &str = ".gc-last";

fn ran_recently(home_ecp: &std::path::Path) -> bool {
    std::fs::metadata(home_ecp.join(LAST_GC_STAMP))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age.as_secs() < AUTO_GC_INTERVAL_SECS)
}

/// Repos are swept in parallel, never one repo by two workers: each repo's
/// dirs live under its own root, so the workers' unlinks never contend for
/// one directory's lock, and each repo's two git calls overlap too. Kept
/// small because gc runs in the background beside a foreground index build.
/// Measured 2026-10-15 on a copy of a 4.0 GB `~/.ecp` (126 repos, release):
/// first sweep 0.88 s vs 2.33 s with one worker (993 dirs, 3.2 GB freed,
/// identical leftovers), steady run 0.22 s vs 0.47 s, RSS 43-51 MB.
const GC_REPO_WORKERS: usize = 4;

fn sweep_repos(repo_roots: &[std::path::PathBuf]) -> usize {
    use rayon::prelude::*;
    let sweep = |repo_root: &std::path::PathBuf| -> usize {
        [
            crate::admin::gc::sweep_stale_generations(repo_root),
            crate::admin::gc::sweep_subtree_builds(repo_root),
            crate::admin::gc::sweep_sessions(repo_root),
            crate::admin::gc::sweep_stale_parse_caches(repo_root),
            crate::admin::gc::sweep_unreachable_commits(repo_root),
        ]
        .into_iter()
        .filter_map(Result::ok)
        .map(|s| s.removed)
        .sum()
    };
    match rayon::ThreadPoolBuilder::new()
        .num_threads(GC_REPO_WORKERS)
        .build()
    {
        Ok(pool) => pool.install(|| repo_roots.par_iter().map(sweep).sum()),
        Err(_) => repo_roots.iter().map(sweep).sum(),
    }
}

pub fn run(args: GcArgs) -> Result<(), ecp_core::EcpError> {
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    // Every trigger (SessionStart, the first query after an upgrade, `ecp
    // update`, a manual run) funnels through here, so this one lock keeps two
    // sweeps from racing to retire the same dir. Not `.prune.lock`: the
    // SessionStart wrapper holds that one while it runs this command.
    let _ = std::fs::create_dir_all(&home_ecp);
    let Ok(_gc_lock) = ecp_core::registry::FileLock::try_exclusive(&home_ecp.join(".gc.lock"))
    else {
        println!("gc: another gc is running; skipped");
        return Ok(());
    };
    if args.auto && ran_recently(&home_ecp) {
        println!("gc: last run under 12 h ago; skipped");
        return Ok(());
    }
    let mut total_removed = 0usize;

    // L1: top-level retired repo dirs.
    if !args.dry_run {
        match crate::admin::gc::sweep_retired_repos(&home_ecp) {
            Ok(s) => total_removed += s.removed,
            Err(e) => eprintln!("gc: sweep_retired_repos: {e}"),
        }
        // Orphaned `<name>.<pid>.<n>.tmp` from interrupted atomic writes.
        match crate::admin::gc::sweep_orphan_tmp(&home_ecp) {
            Ok(s) => total_removed += s.removed,
            Err(e) => eprintln!("gc: sweep_orphan_tmp: {e}"),
        }
        // Ghost registry entries: registered repo whose index dir is gone.
        match ecp_core::registry::RegistryFile::prune_ghost_entries(&home_ecp) {
            Ok(ghosts) => total_removed += ghosts.len(),
            Err(e) => eprintln!("gc: prune_ghost_entries: {e}"),
        }
        // Private git-status indexes of worktrees that no longer exist.
        #[cfg(target_os = "linux")]
        match crate::git::status::sweep_orphans(&home_ecp) {
            Ok(removed) => total_removed += removed,
            Err(e) => eprintln!("gc: sweep_git_index: {e}"),
        }
    }

    // L2 + L3: per-repo generation convergence + session sweep.
    let mut repo_roots = Vec::new();
    if let Ok(it) = std::fs::read_dir(&home_ecp) {
        for entry in it.flatten() {
            let repo_root = entry.path();
            if !repo_root.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || crate::admin::gc::is_retired_dir(&name) {
                continue;
            }
            if name == "telemetry" {
                // Prune cli-calls.jsonl per retention window in each repo subdir.
                // calls.jsonl (MCP) is intentionally left untouched by prune_retention.
                if !args.dry_run {
                    if let Ok(subs) = std::fs::read_dir(&repo_root) {
                        for sub in subs.flatten() {
                            if sub.path().is_dir() {
                                crate::commands::usage::prune_retention(&sub.path());
                            }
                        }
                    }
                }
                continue;
            }
            if !args.dry_run {
                repo_roots.push(repo_root);
            }
        }
    }
    total_removed += sweep_repos(&repo_roots);

    if !args.dry_run {
        let _ = std::fs::write(home_ecp.join(LAST_GC_STAMP), b"");
    }
    println!("gc: removed {total_removed} stale/retired dirs");
    Ok(())
}
