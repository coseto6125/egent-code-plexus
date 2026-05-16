use crate::registry::dirname::SourceType;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitBuildMeta {
    pub version: u32,
    pub sha: String,
    pub source_type: SourceType,
    pub source_id: Option<String>,
    pub built_from_worktree: String,
    pub built_at: String,
    pub parent_sha: Option<String>,
    pub node_count: u32,
    pub embedding_status: EmbeddingStatus,
    pub refs_at_build: Vec<RefRecord>,
    #[serde(default)]
    pub refs_seen_since: Vec<RefRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefRecord {
    pub ref_name: String,
    pub seen_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbeddingStatus {
    None,
    Skipped,
    Computed,
}

impl CommitBuildMeta {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        crate::registry::io::atomic_write_json(path, value)
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}
