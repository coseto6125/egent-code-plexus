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

    pub fn read_or_empty(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(RegistryFile::empty());
        }
        let bytes = fs::read(path)?;
        // Probe the version field before a full parse so old schemas get a
        // clear rejection message rather than a confusing type-mismatch error.
        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }
        if let Ok(probe) = serde_json::from_slice::<VersionProbe>(&bytes) {
            if probe.version != CURRENT_VERSION {
                return Err(io::Error::other(format!(
                    "registry schema v{} (expected v{CURRENT_VERSION}); run `gnx admin reset` to wipe and rebuild",
                    probe.version
                )));
            }
        }
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}

/// Last-resort recovery: walk `~/.gnx/*/meta.json` and rebuild RegistryFile
/// as alias cache. Filesystem is source of truth — group memberships are LOST
/// (registry-only data), operator must re-apply via `gnx admin group add`.
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
