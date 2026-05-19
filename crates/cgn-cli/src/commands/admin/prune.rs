use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct PruneArgs {
    /// Sweep all registry entries whose common_dir no longer exists.
    /// Mutually exclusive with --branch / --repo.
    #[arg(long, conflicts_with_all = ["branch", "repo"])]
    pub orphans: bool,

    /// Target branch to prune (legacy flag; no-op in v2 — branch is not stored).
    #[arg(long, required_unless_present = "orphans")]
    pub branch: Option<String>,

    /// Target repo path (required unless --orphans).
    #[arg(long, required_unless_present = "orphans")]
    pub repo: Option<PathBuf>,
}

pub fn run(args: PruneArgs) -> Result<(), cgn_core::GnxError> {
    if args.orphans {
        return run_orphan_sweep();
    }

    // TODO(phase-5-rewire): per-commit prune — v2 stores commits, not branches.
    // Branch-based prune has no meaning in v2. Use `gnx admin gc` once
    // per-commit GC (Phase 5+) is implemented.
    Err(cgn_core::GnxError::Output(
        "gnx admin prune --branch is a no-op in v2 (branch is not stored). \
         Use `gnx admin prune --orphans` to sweep repos whose worktree is gone."
            .into(),
    ))
}

fn run_orphan_sweep() -> Result<(), cgn_core::GnxError> {
    let home_gnx = cgn_core::registry::resolve_home_gnx();
    run_orphan_sweep_in(&home_gnx)
}

fn run_orphan_sweep_in(home_gnx: &std::path::Path) -> Result<(), cgn_core::GnxError> {
    let audit = cgn_core::registry::AuditLog::open(&home_gnx.join("audit.log")).ok();
    let lock_path = home_gnx.join("registry.json.lock");
    let _lock = cgn_core::registry::FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| cgn_core::GnxError::InvalidArgument(format!("flock: {e}")))?;

    let registry_path = home_gnx.join("registry.json");
    let mut registry = cgn_core::registry::RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| cgn_core::GnxError::InvalidArgument(format!("registry read: {e}")))?;
    let mut orphan_names: Vec<String> = Vec::new();

    // v2: orphan = common_dir no longer exists on disk.
    for (dir_name, alias) in &registry.repos {
        let common_dir = std::path::Path::new(&alias.common_dir);
        if !common_dir.exists() {
            let index_root = home_gnx.join(dir_name);
            match std::fs::remove_dir_all(&index_root) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            orphan_names.push(dir_name.clone());
        }
    }

    if !orphan_names.is_empty() {
        registry
            .repos
            .retain(|k, _v| !orphan_names.iter().any(|name| name == k));
        for group in &mut registry.groups {
            group
                .members
                .retain(|member| !orphan_names.iter().any(|name| name == member));
        }
        cgn_core::registry::RegistryFile::write_atomic(&registry_path, &registry).map_err(
            |e| cgn_core::GnxError::InvalidArgument(format!("write registry: {e}")),
        )?;
    }

    for repo_name in orphan_names {
        if let Some(audit) = &audit {
            let _ = audit.append(&cgn_core::registry::AuditEvent::HookFired {
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
    use cgn_core::registry::{GroupEntry, RegistryFile, RepoAlias};
    use std::collections::BTreeMap;

    #[test]
    fn orphan_sweep_removes_repo_group_member_and_index_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home_gnx = dir.path();
        let dir_name = "orphan-repo__aabbccdd";
        let index_root = home_gnx.join(dir_name);
        let commits_dir = index_root.join("commits").join("sha_abc12345");
        std::fs::create_dir_all(&commits_dir).expect("commits dir");
        std::fs::write(commits_dir.join("graph.bin"), b"graph").expect("graph");

        let missing_common = home_gnx.join("missing-common-dir").join(".git");
        let mut repos = BTreeMap::new();
        repos.insert(
            dir_name.into(),
            RepoAlias {
                dir_name: dir_name.into(),
                common_dir: missing_common.to_string_lossy().into_owned(),
                remote_url: Some("https://example.test/orphan-repo.git".into()),
                aliases: vec!["orphan-repo".into()],
                last_touched: "2026-05-16T00:00:00Z".into(),
                groups: vec!["stale".into()],
            },
        );
        RegistryFile::write_atomic(
            &home_gnx.join("registry.json"),
            &RegistryFile {
                version: 2,
                repos,
                groups: vec![GroupEntry {
                    name: "stale".into(),
                    members: vec![dir_name.into()],
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
