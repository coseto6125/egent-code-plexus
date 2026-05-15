use clap::Subcommand;
use graph_nexus_core::registry::{resolve_home_gnx, FileLock, GroupEntry, RegistryFile};
use graph_nexus_core::GnxError;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum GroupCommands {
    /// Add a repo to a group (auto-creates group)
    Add { repo: String, group: String },
    /// Remove a repo from a group (auto-deletes empty group)
    Remove { repo: String, group: String },
}

pub fn run(cmd: GroupCommands) -> Result<(), GnxError> {
    match cmd {
        GroupCommands::Add { repo, group } => add(&repo, &group),
        GroupCommands::Remove { repo, group } => remove(&repo, &group),
    }
}

fn home_gnx() -> PathBuf {
    resolve_home_gnx()
}

fn mutate_registry<F>(op: F) -> Result<(), GnxError>
where
    F: FnOnce(&mut RegistryFile) -> Result<(), GnxError>,
{
    let gnx = home_gnx();
    let lock_path = gnx.join("registry.json.lock");
    let _lock = FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| GnxError::InvalidArgument(format!("flock: {e}")))?;

    let registry_path = gnx.join("registry.json");
    let mut reg = RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| GnxError::InvalidArgument(format!("registry read: {e}")))?;

    op(&mut reg)?;

    RegistryFile::write_atomic(&registry_path, &reg)
        .map_err(|e| GnxError::InvalidArgument(format!("registry write: {e}")))?;
    Ok(())
}

fn add(repo: &str, group: &str) -> Result<(), GnxError> {
    mutate_registry(|reg| {
        // 1. Update repo.groups (idempotent).
        let repo_entry = reg
            .repos
            .iter_mut()
            .find(|r| r.name == repo)
            .ok_or_else(|| {
                GnxError::Output(format!(
                    "repo not found in registry: {repo}\n\
                     → register the repo first with `gnx admin index`"
                ))
            })?;
        if !repo_entry.groups.iter().any(|g| g == group) {
            repo_entry.groups.push(group.to_string());
        }

        // 2. Update group.members (auto-create group if missing).
        if let Some(g) = reg.groups.iter_mut().find(|g| g.name == group) {
            if !g.members.iter().any(|m| m == repo) {
                g.members.push(repo.to_string());
            }
        } else {
            reg.groups.push(GroupEntry {
                name: group.to_string(),
                members: vec![repo.to_string()],
            });
        }

        println!("✓ Added \"{repo}\" to group \"{group}\"");
        Ok(())
    })
}

fn remove(repo: &str, group: &str) -> Result<(), GnxError> {
    mutate_registry(|reg| {
        // 1. Strip group from repo.groups.
        if let Some(r) = reg.repos.iter_mut().find(|r| r.name == repo) {
            r.groups.retain(|g| g != group);
        }

        // 2. Strip repo from group.members; auto-delete group if now empty.
        if let Some(pos) = reg.groups.iter().position(|g| g.name == group) {
            reg.groups[pos].members.retain(|m| m != repo);
            if reg.groups[pos].members.is_empty() {
                reg.groups.remove(pos);
                println!("✓ Removed \"{repo}\" from \"{group}\" (group auto-deleted, was empty)");
            } else {
                println!("✓ Removed \"{repo}\" from \"{group}\"");
            }
        } else {
            println!("✓ \"{repo}\" was not in group \"{group}\" (no-op)");
        }

        Ok(())
    })
}
