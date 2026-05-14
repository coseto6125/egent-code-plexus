use crate::git_state;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct CleanArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,

    #[arg(long, default_value_t = false)]
    pub all: bool,
}

pub fn run(args: CleanArgs) -> Result<(), graph_nexus_core::GnxError> {
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let mut registry = graph_nexus_core::registry::Registry::open(&home_gnx)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("registry: {e}")))?;

    if args.all {
        let snapshot = registry.snapshot().clone();
        for repo in snapshot.repos {
            let index_dir = home_gnx.join(&repo.name);
            if index_dir.exists() {
                std::fs::remove_dir_all(&index_dir)?;
            }
            let mut new_repo = repo.clone();
            new_repo.branches.clear();
            registry
                .upsert_repo(new_repo)
                .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("upsert: {e}")))?;
        }

        if let Ok(audit) = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
            let _ = audit.append(&graph_nexus_core::registry::AuditEvent::HookFired {
                kind: "clean".into(),
                from: None,
                to: None,
                repo: "all".into(),
            });
        }
    } else {
        let state = git_state::resolve(&args.repo)
            .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("git_state: {e}")))?;

        let index_dir = home_gnx.join(&state.repo_name);
        if index_dir.exists() {
            std::fs::remove_dir_all(&index_dir)?;
        }

        if let Some(repo) = registry
            .snapshot()
            .repos
            .iter()
            .find(|r| r.name == state.repo_name)
            .cloned()
        {
            let mut new_repo = repo;
            new_repo.branches.clear();
            registry
                .upsert_repo(new_repo)
                .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("upsert: {e}")))?;
        }

        if let Ok(audit) = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
            let _ = audit.append(&graph_nexus_core::registry::AuditEvent::HookFired {
                kind: "clean".into(),
                from: None,
                to: None,
                repo: state.repo_name,
            });
        }
    }

    Ok(())
}
