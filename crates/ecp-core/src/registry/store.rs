//! registry.json schema and atomic IO.

use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

pub const CURRENT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryFile {
    pub version: u32,
    #[serde(default)]
    pub repos: BTreeMap<String, RepoAlias>,
    #[serde(default)]
    pub groups: Vec<GroupEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoAlias {
    pub dir_name: String,
    pub common_dir: String,
    pub remote_url: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub last_touched: String,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEntry {
    pub name: String,
    pub members: Vec<String>,
}

impl RepoAlias {
    /// Project a per-repo `RepoMeta` (filesystem source of truth) into a
    /// registry entry. Build paths and the `admin index` self-heal path
    /// share this — keeping the field mapping in one place ensures any
    /// future `RepoAlias` field added gets populated from `RepoMeta`
    /// consistently.
    ///
    /// `groups` is left empty; group membership is owned by `admin group
    /// add/remove` and merged in by [`crate::registry::Registry::upsert_repo`].
    pub fn from_repo_meta(dir_name: impl Into<String>, rm: &crate::registry::RepoMeta) -> Self {
        Self {
            dir_name: dir_name.into(),
            common_dir: rm.common_dir.clone(),
            remote_url: rm.remote_url.clone(),
            aliases: rm.aliases.clone(),
            last_touched: rm.last_touched.clone(),
            groups: vec![],
        }
    }
}

impl RegistryFile {
    pub fn empty() -> Self {
        Self {
            version: CURRENT_VERSION,
            repos: BTreeMap::new(),
            groups: vec![],
        }
    }

    pub fn write_atomic(path: &Path, value: &RegistryFile) -> io::Result<()> {
        atomic_write_json(path, value)
    }

    /// Lock-coupled upsert that bypasses [`crate::registry::Registry`]. Used
    /// by write-only callers (build pipeline, `admin index` self-heal) that
    /// would otherwise pay for `Registry::open`'s eager `registry.json` read
    /// only to discard it before the in-lock re-read inside `upsert_repo`.
    ///
    /// Same semantics as [`crate::registry::Registry::upsert_repo`]: holds
    /// exclusive flock for the read-modify-write cycle, preserves
    /// existing `groups` on a known `dir_name`, skips the write when
    /// nothing changed.
    pub fn upsert_repo_atomic(home_ecp: &Path, entry: RepoAlias) -> io::Result<()> {
        let lock_path = home_ecp.join("registry.json.lock");
        let _lock = super::FileLock::acquire_exclusive(&lock_path)?;

        let registry_path = home_ecp.join("registry.json");
        let mut current = RegistryFile::read_or_empty(&registry_path)?;

        let merged = match current.repos.get(&entry.dir_name) {
            Some(existing) => RepoAlias {
                groups: existing.groups.clone(),
                ..entry
            },
            None => entry,
        };
        if current.repos.get(&merged.dir_name) == Some(&merged) {
            return Ok(());
        }
        current.repos.insert(merged.dir_name.clone(), merged);
        RegistryFile::write_atomic(&registry_path, &current)
    }

    /// Lock-coupled removal of "ghost" entries: a repo registered in
    /// `registry.json` whose index dir (`<home_ecp>/<dir_name>`) no longer
    /// exists on disk. A build that published its registry entry but whose
    /// index dir was later removed out-of-band (interrupted publish, manual
    /// `rm`, a racing writer that clobbered a sibling's dir) leaves the
    /// registry pointing at nothing — every query for that repo then fails to
    /// load. This is the mirror of `prune --orphans` (which keys on a missing
    /// *worktree* `common_dir`); here we key on the missing *index* dir.
    ///
    /// Holds the exclusive registry flock across read-modify-write so a
    /// concurrent `upsert_repo_atomic` can't interleave. Returns the removed
    /// `dir_name`s. Skips the write when nothing is ghosted.
    pub fn prune_ghost_entries(home_ecp: &Path) -> io::Result<Vec<String>> {
        let lock_path = home_ecp.join("registry.json.lock");
        let _lock = super::FileLock::acquire_exclusive(&lock_path)?;

        let registry_path = home_ecp.join("registry.json");
        let mut current = RegistryFile::read_or_empty(&registry_path)?;

        let ghosts: Vec<String> = current
            .repos
            .keys()
            .filter(|dir_name| !home_ecp.join(dir_name).exists())
            .cloned()
            .collect();
        if ghosts.is_empty() {
            return Ok(ghosts);
        }
        current.repos.retain(|k, _| !ghosts.contains(k));
        for group in &mut current.groups {
            group.members.retain(|m| !ghosts.contains(m));
        }
        RegistryFile::write_atomic(&registry_path, &current)?;
        Ok(ghosts)
    }

    pub fn read_or_empty(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(RegistryFile::empty()),
            Err(e) => return Err(e),
        };
        // Probe the version field before a full parse: stale schemas auto-migrate
        // via `rebuild_from_disk` instead of hard-failing.
        // Trade-off: group memberships are registry-only and get wiped — operator
        // must re-apply via `ecp admin group add`. This is preferred over forcing
        // every CLI invocation to error until manual intervention.
        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }
        if let Ok(probe) = serde_json::from_slice::<VersionProbe>(&bytes) {
            if probe.version != CURRENT_VERSION {
                let home_ecp = path
                    .parent()
                    .ok_or_else(|| io::Error::other("registry path has no parent directory"))?;
                let rebuilt = RegistryFile::rebuild_from_disk(home_ecp)?;
                atomic_write_json(path, &rebuilt)?;
                eprintln!(
                    "registry.migrated from=v{} to=v{CURRENT_VERSION} repos={} groups_lost=true",
                    probe.version,
                    rebuilt.repos.len()
                );
                return Ok(rebuilt);
            }
        }
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}

/// Last-resort recovery: walk `~/.ecp/*/meta.json` and rebuild RegistryFile
/// as alias cache. Filesystem is source of truth — group memberships are LOST
/// (registry-only data), operator must re-apply via `ecp admin group add`.
impl RegistryFile {
    pub fn rebuild_from_disk(home_ecp: &Path) -> io::Result<Self> {
        use crate::registry::repo_meta::RepoMeta;

        let mut repos = BTreeMap::new();
        let it = match fs::read_dir(home_ecp) {
            Ok(d) => d,
            Err(_) => return Ok(RegistryFile::empty()),
        };
        for entry in it.flatten() {
            let dir_name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if dir_name.starts_with('_') || dir_name.starts_with('.') {
                continue;
            }
            let repo_meta_path = entry.path().join("meta.json");
            if !repo_meta_path.exists() {
                continue;
            }
            let rm = match RepoMeta::read(&repo_meta_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            repos.insert(
                dir_name.clone(),
                RepoAlias {
                    dir_name,
                    common_dir: rm.common_dir,
                    remote_url: rm.remote_url,
                    aliases: rm.aliases,
                    last_touched: rm.last_touched,
                    groups: vec![],
                },
            );
        }
        Ok(RegistryFile {
            version: CURRENT_VERSION,
            repos,
            groups: vec![],
        })
    }
}

/// Remove user:pass from a remote URL.
pub fn strip_credentials(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(dir_name: &str) -> RepoAlias {
        RepoAlias {
            dir_name: dir_name.into(),
            common_dir: format!("/nonexistent/{dir_name}/.git"),
            remote_url: None,
            aliases: vec![],
            last_touched: "2026-05-27T00:00:00Z".into(),
            groups: vec!["g1".into()],
        }
    }

    #[test]
    fn prune_ghost_entries_removes_registered_repo_with_missing_index_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();

        // `live` has an index dir on disk; `ghost` does not.
        fs::create_dir(home.join("live__aaaa")).expect("mkdir live");
        let mut repos = BTreeMap::new();
        repos.insert("live__aaaa".into(), alias("live__aaaa"));
        repos.insert("ghost__bbbb".into(), alias("ghost__bbbb"));
        RegistryFile::write_atomic(
            &home.join("registry.json"),
            &RegistryFile {
                version: CURRENT_VERSION,
                repos,
                groups: vec![GroupEntry {
                    name: "g1".into(),
                    members: vec!["live__aaaa".into(), "ghost__bbbb".into()],
                }],
            },
        )
        .expect("write");

        let ghosts = RegistryFile::prune_ghost_entries(home).expect("prune");

        assert_eq!(ghosts, vec!["ghost__bbbb".to_string()]);
        let reg = RegistryFile::read_or_empty(&home.join("registry.json")).expect("read");
        assert!(reg.repos.contains_key("live__aaaa"), "live entry kept");
        assert!(!reg.repos.contains_key("ghost__bbbb"), "ghost removed");
        assert_eq!(
            reg.groups[0].members,
            vec!["live__aaaa".to_string()],
            "ghost also dropped from group membership"
        );
    }

    #[test]
    fn prune_ghost_entries_noop_when_all_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        fs::create_dir(home.join("live__aaaa")).expect("mkdir");
        let mut repos = BTreeMap::new();
        repos.insert("live__aaaa".into(), alias("live__aaaa"));
        RegistryFile::write_atomic(
            &home.join("registry.json"),
            &RegistryFile {
                version: CURRENT_VERSION,
                repos,
                groups: vec![],
            },
        )
        .expect("write");

        let ghosts = RegistryFile::prune_ghost_entries(home).expect("prune");
        assert!(ghosts.is_empty(), "no ghosts when index dir exists");
    }
}
