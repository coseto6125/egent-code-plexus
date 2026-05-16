use crate::git_state;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct PruneArgs {
    /// Sweep all registry entries whose worktree_path no longer exists.
    /// Mutually exclusive with --branch / --repo.
    #[arg(long, conflicts_with_all = ["branch", "repo"])]
    pub orphans: bool,

    /// Target branch to prune (required unless --orphans).
    #[arg(long, required_unless_present = "orphans")]
    pub branch: Option<String>,

    /// Target repo path (required unless --orphans).
    #[arg(long, required_unless_present = "orphans")]
    pub repo: Option<PathBuf>,
}

pub fn run(args: PruneArgs) -> Result<(), graph_nexus_core::GnxError> {
    if args.orphans {
        return run_orphan_sweep();
    }

    let branch = args
        .branch
        .ok_or_else(|| graph_nexus_core::GnxError::InvalidArgument("branch required".into()))?;
    let repo = args
        .repo
        .ok_or_else(|| graph_nexus_core::GnxError::InvalidArgument("repo required".into()))?;

    let state = git_state::resolve(&repo)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("git_state: {e}")))?;

    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();

    let branch_seg = graph_nexus_core::registry::sanitize_branch(&branch)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("branch: {e}")))?;
    let index_dir = home_gnx.join(&state.repo_name).join(&branch_seg);
    if index_dir.exists() {
        std::fs::remove_dir_all(&index_dir)?;
    }

    let mut registry = graph_nexus_core::registry::Registry::open(&home_gnx)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("registry: {e}")))?;
    if let Some(repo_entry) = registry
        .snapshot()
        .repos
        .iter()
        .find(|r| r.name == state.repo_name)
        .cloned()
    {
        let mut new_repo = repo_entry;
        new_repo.branches.retain(|b| b.name != branch);
        registry
            .upsert_repo(new_repo)
            .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("upsert: {e}")))?;
    }

    if let Ok(audit) = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
        let _ = audit.append(&graph_nexus_core::registry::AuditEvent::HookFired {
            kind: "prune".into(),
            from: Some(branch.clone()),
            to: None,
            repo: state.repo_name,
        });
    }
    Ok(())
}

fn run_orphan_sweep() -> Result<(), graph_nexus_core::GnxError> {
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    run_orphan_sweep_in(&home_gnx)
}

fn run_orphan_sweep_in(home_gnx: &std::path::Path) -> Result<(), graph_nexus_core::GnxError> {
    let audit = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")).ok();
    let lock_path = home_gnx.join("registry.json.lock");
    let _lock = graph_nexus_core::registry::FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("flock: {e}")))?;

    let registry_path = home_gnx.join("registry.json");
    let mut registry = graph_nexus_core::registry::RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("registry read: {e}")))?;
    let mut orphan_names = Vec::new();

    for repo in &registry.repos {
        let worktree_path = std::path::Path::new(&repo.worktree_path);
        if !worktree_path.exists() {
            let index_root = std::path::Path::new(&repo.index_dir_root);
            match std::fs::remove_dir_all(index_root) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            orphan_names.push(repo.name.clone());
        }
    }

    if !orphan_names.is_empty() {
        registry
            .repos
            .retain(|repo| !orphan_names.iter().any(|name| name == &repo.name));
        for group in &mut registry.groups {
            group
                .members
                .retain(|member| !orphan_names.iter().any(|name| name == member));
        }
        graph_nexus_core::registry::RegistryFile::write_atomic(&registry_path, &registry).map_err(
            |e| graph_nexus_core::GnxError::InvalidArgument(format!("write registry: {e}")),
        )?;
    }

    for repo_name in orphan_names {
        if let Some(audit) = &audit {
            let _ = audit.append(&graph_nexus_core::registry::AuditEvent::HookFired {
                kind: "prune-orphan".into(),
                from: Some(repo_name.clone()),
                to: None,
                repo: repo_name,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::registry::{BranchEntry, GroupEntry, RegistryFile, RepoEntry};

    #[test]
    fn orphan_sweep_removes_repo_group_member_and_index_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home_gnx = dir.path();
        let index_root = home_gnx.join("orphan-repo");
        let branch_dir = index_root.join("main");
        std::fs::create_dir_all(&branch_dir).expect("index root");
        std::fs::write(branch_dir.join("graph.bin"), b"graph").expect("graph");

        RegistryFile::write_atomic(
            &home_gnx.join("registry.json"),
            &RegistryFile {
                version: 1,
                repos: vec![RepoEntry {
                    name: "orphan-repo".into(),
                    remote_url: "https://example.test/orphan-repo.git".into(),
                    worktree_path: home_gnx
                        .join("missing-worktree")
                        .to_string_lossy()
                        .into_owned(),
                    index_dir_root: index_root.to_string_lossy().into_owned(),
                    branches: vec![BranchEntry {
                        name: "main".into(),
                        index_dir: branch_dir.to_string_lossy().into_owned(),
                        indexed_at: "2026-05-16T00:00:00Z".into(),
                        node_count: 1,
                        delta_size: 0,
                        embedding_status: "none".into(),
                    }],
                    groups: vec!["stale".into()],
                }],
                groups: vec![GroupEntry {
                    name: "stale".into(),
                    members: vec!["orphan-repo".into()],
                }],
            },
        )
        .expect("registry");

        run_orphan_sweep_in(home_gnx).expect("sweep");

        let registry = RegistryFile::read_or_empty(&home_gnx.join("registry.json")).expect("read");
        assert!(registry.repos.is_empty());
        assert!(registry.groups[0].members.is_empty());
        assert!(!index_root.exists());
    }
}
