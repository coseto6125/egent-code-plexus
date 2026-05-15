//! `gnx remove <target>` — delete one indexed repo by name / alias / path.
//!
//! Complements `gnx clean` (which targets the CURRENT repo only) by letting
//! the operator drop ANY registered repo regardless of cwd. Matching order:
//!   1. exact `RepoEntry.name` (the registry slug, e.g. `gitnexus-rs-main`)
//!   2. exact `RepoEntry.name` again as alias semantics — same field today
//!   3. `RepoEntry.worktree_path` when `target` is an absolute path
//!
//! Side effects per removal:
//!   * delete `<home_gnx>/<repo.name>/` (index dir, all branches)
//!   * rewrite `registry.json` without that entry (atomic, flock-guarded)
//!   * append a `registry.mutate` audit event
//!
//! Scope note: `Registry` exposes no `remove_repo` API, so we read the
//! snapshot, filter, and `RegistryFile::write_atomic` under exclusive
//! `FileLock` — same recipe `Registry::upsert_repo` uses internally.

use clap::Args;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct RemoveArgs {
    /// Registry name, alias, or absolute worktree path of the repo to delete.
    pub target: String,

    /// Reserved for future interactive confirmation. The CLI is non-
    /// interactive by design (LLM-friendly), so this flag is informational.
    #[arg(long, default_value_t = false)]
    pub force: bool,
}

pub fn run(args: RemoveArgs) -> Result<(), graph_nexus_core::GnxError> {
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let registry = graph_nexus_core::registry::Registry::open(&home_gnx)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("registry: {e}")))?;

    let snapshot = registry.snapshot().clone();
    // Drop the registry handle to release any reader state before we
    // grab the exclusive flock ourselves.
    drop(registry);

    let matches = find_matches(&snapshot.repos, &args.target);

    match matches.len() {
        0 => Err(graph_nexus_core::GnxError::InvalidArgument(format!(
            "no registered repo matches '{}' — try `gnx list` to see available names",
            args.target
        ))),
        1 => {
            let target_name = matches[0].clone();
            let target_repo = snapshot
                .repos
                .iter()
                .find(|r| r.name == target_name)
                .expect("matched name must still be in snapshot")
                .clone();

            // 1. Delete the index directory if present.
            let index_dir = home_gnx.join(&target_repo.name);
            if index_dir.exists() {
                std::fs::remove_dir_all(&index_dir)?;
            }

            // 2. Filter-and-rewrite registry.json under exclusive flock.
            //    Re-read inside the lock to avoid losing concurrent updates.
            rewrite_without(&home_gnx, &target_repo.name)?;

            // 3. Audit-log the removal.
            if let Ok(audit) =
                graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log"))
            {
                let _ = audit.append(&graph_nexus_core::registry::AuditEvent::RegistryMutate {
                    op: "remove".into(),
                    repo: target_repo.name.clone(),
                    branch: None,
                });
            }

            println!(
                "removed {} (was at {})",
                target_repo.name, target_repo.worktree_path
            );
            Ok(())
        }
        _ => Err(graph_nexus_core::GnxError::InvalidArgument(format!(
            "ambiguous target '{}' matched {} repos: {} — pass an exact registry name",
            args.target,
            matches.len(),
            matches.join(", ")
        ))),
    }
}

/// Return distinct repo names whose `name` or `worktree_path` matches
/// `target`. Match rules:
///   * `name` is compared as an exact string (covers both registry slug
///     and current `alias` semantics — both live on the `name` field).
///   * `worktree_path` is compared as an exact string when `target` looks
///     like an absolute path. Falling back to name lookup avoids treating
///     `feature/x` as a path on Windows-ish inputs.
fn find_matches(repos: &[graph_nexus_core::registry::RepoEntry], target: &str) -> Vec<String> {
    let target_path = Path::new(target);
    let path_lookup = target_path.is_absolute();
    let mut matches: Vec<String> = Vec::new();
    for r in repos {
        let by_name = r.name == target;
        let by_path = path_lookup && PathBuf::from(&r.worktree_path).as_path() == target_path;
        if (by_name || by_path) && !matches.contains(&r.name) {
            matches.push(r.name.clone());
        }
    }
    matches
}

/// Re-read registry.json under exclusive flock, drop the entry whose
/// `name == repo_name`, and atomically write back. Mirrors the
/// read-modify-write pattern in `Registry::upsert_repo`.
fn rewrite_without(home_gnx: &Path, repo_name: &str) -> Result<(), graph_nexus_core::GnxError> {
    let lock_path = home_gnx.join("registry.json.lock");
    let _lock = graph_nexus_core::registry::FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("flock: {e}")))?;

    let registry_path = home_gnx.join("registry.json");
    let mut current = graph_nexus_core::registry::RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("read: {e}")))?;

    let before = current.repos.len();
    current.repos.retain(|r| r.name != repo_name);
    // Also remove the repo from any group's membership list to keep
    // registry.json consistent — groups referencing a deleted repo would
    // otherwise dangle until the next group_sync.
    for g in current.groups.iter_mut() {
        g.members.retain(|m| m != repo_name);
    }

    // Only write if we actually changed something. A concurrent removal
    // could have raced us; in that case bail quietly so we don't bump the
    // .bak rotation for no reason.
    if current.repos.len() == before {
        return Ok(());
    }

    graph_nexus_core::registry::RegistryFile::write_atomic(&registry_path, &current)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("write: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::registry::{BranchEntry, RepoEntry};

    fn repo(name: &str, worktree: &str) -> RepoEntry {
        RepoEntry {
            name: name.into(),
            remote_url: "git@x:y/z.git".into(),
            worktree_path: worktree.into(),
            index_dir_root: format!("/h/.gnx/{name}"),
            branches: vec![BranchEntry {
                name: "main".into(),
                index_dir: format!("/h/.gnx/{name}/main"),
                indexed_at: "2026-05-14T00:00:00Z".into(),
                node_count: 1,
                delta_size: 0,
                embedding_status: "skipped".into(),
            }],
            group: None,
        }
    }

    // Pick fixture roots that `Path::is_absolute()` actually accepts on the
    // current platform; "/w/alpha" is absolute on Unix but not on Windows, and
    // the path-matching branch of `find_matches` short-circuits on the
    // `is_absolute()` check.
    #[cfg(windows)]
    const ALPHA_WORKTREE: &str = r"C:\w\alpha";
    #[cfg(windows)]
    const BETA_WORKTREE: &str = r"C:\w\beta";
    #[cfg(not(windows))]
    const ALPHA_WORKTREE: &str = "/w/alpha";
    #[cfg(not(windows))]
    const BETA_WORKTREE: &str = "/w/beta";

    #[test]
    fn find_matches_by_name_returns_single_repo() {
        let repos = vec![repo("alpha", ALPHA_WORKTREE), repo("beta", BETA_WORKTREE)];
        let m = find_matches(&repos, "alpha");
        assert_eq!(m, vec!["alpha".to_string()]);
    }

    #[test]
    fn find_matches_by_absolute_path_resolves_to_name() {
        let repos = vec![repo("alpha", ALPHA_WORKTREE), repo("beta", BETA_WORKTREE)];
        let m = find_matches(&repos, BETA_WORKTREE);
        assert_eq!(m, vec!["beta".to_string()]);
    }

    #[test]
    fn find_matches_relative_path_is_not_treated_as_path() {
        // "alpha/main" is not absolute, so it must not collide with a
        // worktree path lookup — only name match would fire (none here).
        let repos = vec![repo("alpha", ALPHA_WORKTREE)];
        let m = find_matches(&repos, "alpha/main");
        assert!(m.is_empty(), "relative input should not match: {m:?}");
    }

    #[test]
    fn find_matches_returns_empty_for_unknown_target() {
        let repos = vec![repo("alpha", ALPHA_WORKTREE)];
        let m = find_matches(&repos, "nope");
        assert!(m.is_empty());
    }
}
