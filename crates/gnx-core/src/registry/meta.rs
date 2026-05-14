//! Per-branch meta.json — enough to rebuild a registry entry if
//! registry.json AND .bak are both lost. Spec §1, §2.1.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
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
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

impl BranchMeta {
    pub fn write_atomic(path: &Path, value: &BranchMeta) -> io::Result<()> {
        crate::registry::io::atomic_write_json(path, value)
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}

fn default_schema_version() -> u32 {
    1
}
