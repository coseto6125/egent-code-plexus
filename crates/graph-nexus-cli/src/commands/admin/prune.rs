use crate::git_state;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct PruneArgs {
    #[arg(long)]
    pub branch: String,
    #[arg(long)]
    pub repo: PathBuf,
}

pub fn run(args: PruneArgs) -> Result<(), graph_nexus_core::GnxError> {
    let state = git_state::resolve(&args.repo)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("git_state: {e}")))?;

    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();

    let branch_seg = graph_nexus_core::registry::sanitize_branch(&args.branch)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("branch: {e}")))?;
    let index_dir = home_gnx.join(&state.repo_name).join(&branch_seg);
    if index_dir.exists() {
        std::fs::remove_dir_all(&index_dir)?;
    }

    let mut registry = graph_nexus_core::registry::Registry::open(&home_gnx)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("registry: {e}")))?;
    if let Some(repo) = registry
        .snapshot()
        .repos
        .iter()
        .find(|r| r.name == state.repo_name)
        .cloned()
    {
        let mut new_repo = repo;
        new_repo.branches.retain(|b| b.name != args.branch);
        registry
            .upsert_repo(new_repo)
            .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("upsert: {e}")))?;
    }

    if let Ok(audit) = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
        let _ = audit.append(&graph_nexus_core::registry::AuditEvent::HookFired {
            kind: "prune".into(),
            from: Some(args.branch.clone()),
            to: None,
            repo: state.repo_name,
        });
    }
    Ok(())
}
