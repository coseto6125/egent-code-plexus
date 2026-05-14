use crate::git_state;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct RenameBranchArgs {
    #[arg(long)]
    pub from: String,
    #[arg(long)]
    pub to: String,
    #[arg(long)]
    pub repo: PathBuf,
}

pub fn run(args: RenameBranchArgs) -> Result<(), gnx_core::GnxError> {
    let state = git_state::resolve(&args.repo).map_err(|e| {
        gnx_core::GnxError::InvalidArgument(format!("git_state: {e}"))
    })?;

    let home_gnx = gnx_core::registry::resolve_home_gnx();

    let from_seg = gnx_core::registry::sanitize_branch(&args.from).map_err(|e| {
        gnx_core::GnxError::InvalidArgument(format!("from: {e}"))
    })?;
    let to_seg = gnx_core::registry::sanitize_branch(&args.to).map_err(|e| {
        gnx_core::GnxError::InvalidArgument(format!("to: {e}"))
    })?;

    let from_dir = home_gnx.join(&state.repo_name).join(&from_seg);
    let to_dir = home_gnx.join(&state.repo_name).join(&to_seg);

    if from_dir.exists() {
        if to_dir.exists() {
            return Err(gnx_core::GnxError::InvalidArgument(format!(
                "target index dir {to_dir:?} already exists"
            )));
        }
        std::fs::rename(&from_dir, &to_dir)?;
    }

    let mut registry = gnx_core::registry::Registry::open(&home_gnx).map_err(|e| {
        gnx_core::GnxError::InvalidArgument(format!("registry: {e}"))
    })?;
    if let Some(repo) = registry
        .snapshot()
        .repos
        .iter()
        .find(|r| r.name == state.repo_name)
        .cloned()
    {
        let mut new_repo = repo;
        for b in new_repo.branches.iter_mut() {
            if b.name == args.from {
                b.name = args.to.clone();
                b.index_dir = to_dir.to_string_lossy().into();
            }
        }
        registry.upsert_repo(new_repo).map_err(|e| {
            gnx_core::GnxError::InvalidArgument(format!("upsert: {e}"))
        })?;
    }

    if let Ok(audit) = gnx_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
        let _ = audit.append(&gnx_core::registry::AuditEvent::HookFired {
            kind: "rename".into(),
            from: Some(args.from.clone()),
            to: Some(args.to.clone()),
            repo: state.repo_name,
        });
    }
    Ok(())
}
