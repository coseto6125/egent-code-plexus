use clap::Subcommand;
use cgn_core::registry::{resolve_home_cgn, FileLock, GroupEntry, RegistryFile};
use cgn_core::CgnError;

#[derive(Subcommand, Debug)]
pub enum GroupCommands {
    /// Add a repo to a group (auto-creates group)
    Add { repo: String, group: String },
    /// Remove a repo from a group (auto-deletes empty group)
    Remove { repo: String, group: String },
}

pub fn run(cmd: GroupCommands) -> Result<(), CgnError> {
    match cmd {
        GroupCommands::Add { repo, group } => add(&repo, &group),
        GroupCommands::Remove { repo, group } => remove(&repo, &group),
    }
}

fn mutate_registry<F>(op: F) -> Result<(), CgnError>
where
    F: FnOnce(&mut RegistryFile) -> Result<bool, CgnError>,
{
    let cgn = resolve_home_cgn();
    let lock_path = cgn.join("registry.json.lock");
    let _lock = FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| CgnError::InvalidArgument(format!("flock: {e}")))?;

    let registry_path = cgn.join("registry.json");
    let mut reg = RegistryFile::read_or_empty(&registry_path)
        .map_err(|e| CgnError::InvalidArgument(format!("registry read: {e}")))?;

    let mutated = op(&mut reg)?;

    if mutated {
        RegistryFile::write_atomic(&registry_path, &reg)
            .map_err(|e| CgnError::InvalidArgument(format!("registry write: {e}")))?;
    }
    Ok(())
}

fn add(repo: &str, group: &str) -> Result<(), CgnError> {
    mutate_registry(|reg| {
        let mut changed = false;

        // 1. Update alias.groups (idempotent).
        // Match by dir_name or any alias name.
        let repo_entry = reg
            .repos
            .iter_mut()
            .find(|(_k, v)| v.dir_name == repo || v.aliases.iter().any(|a| a == repo))
            .map(|(_k, v)| v)
            .ok_or_else(|| {
                CgnError::Output(format!(
                    "repo not found in registry: {repo}\n\
                     → register the repo first with `cgn admin index`"
                ))
            })?;
        if !repo_entry.groups.iter().any(|g| g == group) {
            repo_entry.groups.push(group.to_string());
            changed = true;
        }

        // 2. Update group.members (auto-create group if missing).
        if let Some(g) = reg.groups.iter_mut().find(|g| g.name == group) {
            if !g.members.iter().any(|m| m == repo) {
                g.members.push(repo.to_string());
                changed = true;
            }
        } else {
            reg.groups.push(GroupEntry {
                name: group.to_string(),
                members: vec![repo.to_string()],
            });
            changed = true;
        }

        if changed {
            println!("✓ Added \"{repo}\" to group \"{group}\"");
        }
        Ok(changed)
    })
}

fn remove(repo: &str, group: &str) -> Result<(), CgnError> {
    mutate_registry(|reg| {
        let mut changed = false;

        // 1. Strip group from alias.groups.
        if let Some((_k, r)) = reg
            .repos
            .iter_mut()
            .find(|(_k, v)| v.dir_name == repo || v.aliases.iter().any(|a| a == repo))
        {
            let before = r.groups.len();
            r.groups.retain(|g| g != group);
            if r.groups.len() != before {
                changed = true;
            }
        }

        // 2. Strip repo from group.members; auto-delete group if now empty.
        if let Some(pos) = reg.groups.iter().position(|g| g.name == group) {
            let before = reg.groups[pos].members.len();
            reg.groups[pos].members.retain(|m| m != repo);
            if reg.groups[pos].members.len() != before {
                changed = true;
            }
            if reg.groups[pos].members.is_empty() {
                reg.groups.remove(pos);
                println!("✓ Removed \"{repo}\" from \"{group}\" (group auto-deleted, was empty)");
            } else if changed {
                println!("✓ Removed \"{repo}\" from \"{group}\"");
            }
        } else if !changed {
            println!("✓ \"{repo}\" was not in group \"{group}\" (no-op)");
        }

        Ok(changed)
    })
}
