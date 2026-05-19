//! registry.json schema and atomic IO. Spec §2.

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
    pub fn upsert_repo_atomic(home_gnx: &Path, entry: RepoAlias) -> io::Result<()> {
        let lock_path = home_gnx.join("registry.json.lock");
        let _lock = super::FileLock::acquire_exclusive(&lock_path)?;

        let registry_path = home_gnx.join("registry.json");
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

    pub fn read_or_empty(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(RegistryFile::empty());
        }
        let bytes = fs::read(path)?;
        // Probe the version field before a full parse: stale schemas auto-migrate
        // via `rebuild_from_disk` (spec §12 recovery) instead of hard-failing.
        // Trade-off: group memberships are registry-only and get wiped — operator
        // must re-apply via `cgn admin group add`. This is preferred over forcing
        // every CLI invocation to error until manual intervention.
        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }
        if let Ok(probe) = serde_json::from_slice::<VersionProbe>(&bytes) {
            if probe.version != CURRENT_VERSION {
                let home_gnx = path
                    .parent()
                    .ok_or_else(|| io::Error::other("registry path has no parent directory"))?;
                let rebuilt = RegistryFile::rebuild_from_disk(home_gnx)?;
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

/// Last-resort recovery: walk `~/.gnx/*/meta.json` and rebuild RegistryFile
/// as alias cache. Filesystem is source of truth — group memberships are LOST
/// (registry-only data), operator must re-apply via `cgn admin group add`.
impl RegistryFile {
    pub fn rebuild_from_disk(home_gnx: &Path) -> io::Result<Self> {
        use crate::registry::repo_meta::RepoMeta;

        let mut repos = BTreeMap::new();
        let it = match fs::read_dir(home_gnx) {
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
