//! registry.json schema and atomic IO. Spec §2.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryFile {
    pub version: u32,
    #[serde(default)]
    pub repos: Vec<RepoEntry>,
    #[serde(default)]
    pub groups: Vec<GroupEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    pub name: String,
    pub remote_url: String,
    pub worktree_path: String,
    pub index_dir_root: String,
    pub branches: Vec<BranchEntry>,
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEntry {
    pub name: String,
    pub index_dir: String,
    pub indexed_at: String,
    pub node_count: u32,
    #[serde(default)]
    pub delta_size: u64,
    pub embedding_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEntry {
    pub name: String,
    pub members: Vec<String>,
}

impl RegistryFile {
    pub fn empty() -> Self {
        Self {
            version: 1,
            repos: vec![],
            groups: vec![],
        }
    }

    /// Atomic write: serialize → tmp → fsync → rename. If `path`
    /// already exists, copy current content to `<path>.bak` first.
    pub fn write_atomic(path: &Path, value: &RegistryFile) -> io::Result<()> {
        if path.exists() {
            let bak = bak_path(path);
            fs::copy(path, &bak)?;
        }

        let tmp = path.with_extension("json.tmp");
        {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)?;
            let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read registry.json. On parse failure, try .bak. On both failures
    /// or missing file, return empty registry.
    pub fn read_or_empty(path: &Path) -> io::Result<Self> {
        match try_read(path) {
            Ok(v) => Ok(v),
            Err(_) => {
                let bak = bak_path(path);
                if bak.exists() {
                    match try_read(&bak) {
                        Ok(v) => Ok(v),
                        Err(_) => Ok(RegistryFile::empty()),
                    }
                } else {
                    Ok(RegistryFile::empty())
                }
            }
        }
    }
}

impl RegistryFile {
    /// Last-resort recovery: walk `~/.gnx/*/*/meta.json` and synthesize
    /// a fresh registry. Group memberships are LOST in this path
    /// (registry-only data) — operator must re-apply via `group_sync`.
    pub fn rebuild_from_disk(home_gnx: &Path) -> io::Result<Self> {
        use crate::registry::meta::BranchMeta;
        use std::collections::HashMap;
        let mut by_repo: HashMap<String, (String, String, String, Vec<BranchEntry>)> =
            HashMap::new();

        let repo_dirs = match fs::read_dir(home_gnx) {
            Ok(it) => it,
            Err(_) => return Ok(RegistryFile::empty()),
        };
        for repo_entry in repo_dirs.flatten() {
            if !repo_entry.path().is_dir() {
                continue;
            }
            let repo_name = match repo_entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            // Skip non-repo entries like _module, audit.log, etc.
            if repo_name.starts_with('_') || repo_name == "registry.json" {
                continue;
            }
            let branches_dir = repo_entry.path();
            let branch_iter = match fs::read_dir(&branches_dir) {
                Ok(it) => it,
                Err(_) => continue,
            };
            for branch_entry in branch_iter.flatten() {
                let meta_path = branch_entry.path().join("meta.json");
                if !meta_path.exists() {
                    continue;
                }
                let meta = match BranchMeta::read(&meta_path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let branch_name = match branch_entry.file_name().into_string() {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let bre = BranchEntry {
                    name: branch_name,
                    index_dir: branch_entry.path().to_string_lossy().into(),
                    indexed_at: meta.indexed_at.clone(),
                    node_count: meta.node_count,
                    delta_size: meta.delta_size,
                    embedding_status: "unknown".into(),
                };
                let entry = by_repo
                    .entry(repo_name.clone())
                    .or_insert_with(|| {
                        (
                            meta.worktree_path.clone(),
                            meta.remote_url.clone(),
                            meta.indexed_at.clone(),
                            vec![],
                        )
                    });
                // If this branch is newer, take its worktree_path / remote_url
                if meta.indexed_at.as_str() > entry.2.as_str() {
                    entry.0 = meta.worktree_path.clone();
                    entry.1 = meta.remote_url.clone();
                    entry.2 = meta.indexed_at.clone();
                }
                entry.3.push(bre);
            }
        }

        let repos = by_repo
            .into_iter()
            .map(|(name, (worktree_path, remote_url, _latest, branches))| RepoEntry {
                name: name.clone(),
                remote_url,
                worktree_path,
                index_dir_root: home_gnx.join(&name).to_string_lossy().into(),
                branches,
                group: None,
            })
            .collect();

        Ok(RegistryFile {
            version: 1,
            repos,
            groups: vec![],
        })
    }
}

fn bak_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    std::path::PathBuf::from(s)
}

fn try_read(path: &Path) -> io::Result<RegistryFile> {
    if !path.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no registry"));
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

/// Remove user:pass from a remote URL. SSH URLs (`git@host:path`) and
/// already-clean URLs pass through unchanged.
pub fn strip_credentials(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}
