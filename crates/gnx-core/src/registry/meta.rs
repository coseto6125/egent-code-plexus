//! Per-branch meta.json — enough to rebuild a registry entry if
//! registry.json AND .bak are both lost. Spec §1, §2.1.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchMeta {
    pub indexed_at: String,
    pub node_count: u32,
    #[serde(default)]
    pub delta_size: u64,
    #[serde(default)]
    pub last_compact_at: Option<String>,
    pub worktree_path: String,
    pub remote_url: String,
    pub schema_version: u32,
}

impl BranchMeta {
    pub fn write_atomic(path: &Path, value: &BranchMeta) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
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

    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}
