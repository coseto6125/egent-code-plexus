//! `cgn admin drop` — delete a repo's on-disk index data AND registry entry.
//!
//! Targets the repo identified by `--repo` (path, defaulting to cwd).
//! Side effects:
//!   * delete `<home_gnx>/<repo.name>/` (index dir, all branches)
//!   * rewrite `registry.json` without that entry (atomic, flock-guarded)
//!   * append a `registry.mutate` audit event
//!
//! Use `--all` to drop every registered repo at once.

use crate::repo_identity::repo_dir_name_for_cwd;
use clap::Args;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct DropArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,

    #[arg(long, default_value_t = false)]
    pub all: bool,
}

pub fn run(args: DropArgs) -> Result<(), cgn_core::GnxError> {
    let home_gnx = cgn_core::registry::resolve_home_gnx();
    let registry = cgn_core::registry::Registry::open(&home_gnx)
        .map_err(|e| cgn_core::GnxError::InvalidArgument(format!("registry: {e}")))?;

    if args.all {
        let snapshot = registry.snapshot().clone();
        for (dir_name, _alias) in &snapshot.repos {
            let index_dir = home_gnx.join(dir_name);
            if index_dir.exists() {
                std::fs::remove_dir_all(&index_dir)?;
            }
        }
        // Rewrite registry removing all entries under exclusive lock.
        drop(registry);
        rewrite_without(&home_gnx, None)?;

        if let Ok(audit) = cgn_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
            let _ = audit.append(&cgn_core::registry::AuditEvent::RegistryMutate {
                op: "drop-all".into(),
                repo: "all".into(),
                branch: None,
            });
        }
    } else {
        // `git_state::resolve` returned the bare basename ("sample_repo"), but the
        // PR #55 layout writes to `<basename>__<sha256(common_dir)[:8]>/` — drop
        // would silently miss the dir and registry entry. `repo_dir_name_for_cwd`
        // is the same helper `build_l2` uses to write the dir, so identifying
        // the target the same way guarantees we find it.
        let dir_name = repo_dir_name_for_cwd(&args.repo)
            .map_err(|e| cgn_core::GnxError::InvalidArgument(format!("repo_identity: {e}")))?;

        let index_dir = home_gnx.join(&dir_name);
        if index_dir.exists() {
            std::fs::remove_dir_all(&index_dir)?;
        }

        // Drop registry handle before acquiring exclusive flock.
        drop(registry);
        rewrite_without(&home_gnx, Some(&dir_name))?;

        if let Ok(audit) = cgn_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
            let _ = audit.append(&cgn_core::registry::AuditEvent::RegistryMutate {
                op: "drop".into(),
                repo: dir_name,
                branch: None,
            });
        }
    }

    Ok(())
}

/// Re-read registry.json under exclusive flock, remove the named repo (or all
/// repos when `repo_name` is None), and atomically write back.
fn rewrite_without(
    home_gnx: &Path,
    repo_name: Option<&str>,
) -> Result<(), cgn_core::GnxError> {
    let lock_path = home_gnx.join("registry.json.lock");
    let _lock = cgn_core::registry::FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| cgn_core::GnxError::InvalidArgument(format!("flock: {e}")))?;

    let registry_path = home_gnx.join("registry.json");
    let mut current = cgn_core::registry::RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| cgn_core::GnxError::InvalidArgument(format!("read: {e}")))?;

    match repo_name {
        Some(name) => {
            current.repos.retain(|k, _v| k != name);
            for g in current.groups.iter_mut() {
                g.members.retain(|m| m != name);
            }
        }
        None => {
            let all_names: Vec<String> = current.repos.keys().cloned().collect();
            current.repos.clear();
            for g in current.groups.iter_mut() {
                g.members.retain(|m| !all_names.contains(m));
            }
        }
    }

    cgn_core::registry::RegistryFile::write_atomic(&registry_path, &current)
        .map_err(|e| cgn_core::GnxError::InvalidArgument(format!("write: {e}")))?;
    Ok(())
}
