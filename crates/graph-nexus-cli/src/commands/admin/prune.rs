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
    let registry = graph_nexus_core::registry::Registry::open(&home_gnx)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("registry: {e}")))?;
    let audit = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")).ok();

    let snapshot = registry.snapshot().clone();
    let mut orphans = Vec::new();

    // Identify and clean orphaned repos
    for repo in &snapshot.repos {
        let wt_path = std::path::Path::new(&repo.worktree_path);
        if !wt_path.exists() {
            orphans.push(repo.clone());
            // Clean up the index dir tree
            let index_root = std::path::Path::new(&repo.index_dir_root);
            if index_root.exists() {
                let _ = std::fs::remove_dir_all(index_root);
            }
        }
    }

    // Remove orphaned repos from registry
    if !orphans.is_empty() {
        let mut updated_snapshot = snapshot.clone();
        updated_snapshot
            .repos
            .retain(|r| !orphans.iter().any(|o| o.name == r.name));
        let registry_path = home_gnx.join("registry.json");
        graph_nexus_core::registry::RegistryFile::write_atomic(&registry_path, &updated_snapshot)
            .map_err(|e| {
                graph_nexus_core::GnxError::InvalidArgument(format!("write registry: {e}"))
            })?;
    }

    // Audit log each orphan drop
    for repo in orphans {
        if let Some(a) = &audit {
            let _ = a.append(&graph_nexus_core::registry::AuditEvent::HookFired {
                kind: "prune-orphan".into(),
                from: Some(repo.name.clone()),
                to: None,
                repo: repo.name.clone(),
            });
        }
    }

    Ok(())
}
