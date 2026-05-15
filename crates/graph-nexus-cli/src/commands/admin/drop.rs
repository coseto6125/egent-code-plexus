//! `gnx admin drop` — delete a repo's on-disk index data AND registry entry.
//!
//! Targets the repo identified by `--repo` (path, defaulting to cwd).
//! Side effects:
//!   * delete `<home_gnx>/<repo.name>/` (index dir, all branches)
//!   * rewrite `registry.json` without that entry (atomic, flock-guarded)
//!   * append a `registry.mutate` audit event
//!
//! Use `--all` to drop every registered repo at once.

use crate::git_state;
use clap::Args;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct DropArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,

    #[arg(long, default_value_t = false)]
    pub all: bool,
}

pub fn run(args: DropArgs) -> Result<(), graph_nexus_core::GnxError> {
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let mut registry = graph_nexus_core::registry::Registry::open(&home_gnx)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("registry: {e}")))?;

    if args.all {
        let snapshot = registry.snapshot().clone();
        for repo in &snapshot.repos {
            let index_dir = home_gnx.join(&repo.name);
            if index_dir.exists() {
                std::fs::remove_dir_all(&index_dir)?;
            }
        }
        // Rewrite registry removing all entries under exclusive lock.
        drop(registry);
        rewrite_without(&home_gnx, None)?;

        if let Ok(audit) = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
            let _ = audit.append(&graph_nexus_core::registry::AuditEvent::RegistryMutate {
                op: "drop-all".into(),
                repo: "all".into(),
                branch: None,
            });
        }
    } else {
        let state = git_state::resolve(&args.repo)
            .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("git_state: {e}")))?;

        let index_dir = home_gnx.join(&state.repo_name);
        if index_dir.exists() {
            std::fs::remove_dir_all(&index_dir)?;
        }

        // Drop registry handle before acquiring exclusive flock.
        let repo_name = state.repo_name.clone();
        drop(registry);
        rewrite_without(&home_gnx, Some(&repo_name))?;

        if let Ok(audit) = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
            let _ = audit.append(&graph_nexus_core::registry::AuditEvent::RegistryMutate {
                op: "drop".into(),
                repo: repo_name,
                branch: None,
            });
        }
    }

    Ok(())
}

/// Re-read registry.json under exclusive flock, remove the named repo (or all
/// repos when `repo_name` is None), and atomically write back.
fn rewrite_without(home_gnx: &Path, repo_name: Option<&str>) -> Result<(), graph_nexus_core::GnxError> {
    let lock_path = home_gnx.join("registry.json.lock");
    let _lock = graph_nexus_core::registry::FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("flock: {e}")))?;

    let registry_path = home_gnx.join("registry.json");
    let mut current = graph_nexus_core::registry::RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("read: {e}")))?;

    match repo_name {
        Some(name) => {
            current.repos.retain(|r| r.name != name);
            for g in current.groups.iter_mut() {
                g.members.retain(|m| m != name);
            }
        }
        None => {
            let all_names: Vec<String> = current.repos.iter().map(|r| r.name.clone()).collect();
            current.repos.clear();
            for g in current.groups.iter_mut() {
                g.members.retain(|m| !all_names.contains(m));
            }
        }
    }

    graph_nexus_core::registry::RegistryFile::write_atomic(&registry_path, &current)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("write: {e}")))?;
    Ok(())
}
