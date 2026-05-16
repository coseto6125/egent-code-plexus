use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub version: u32,
    pub session_id: String,
    pub pid: Option<u32>,
    pub started_at: String,
    pub last_touched: String,
    pub base_sha: String,
    pub source_worktree: String,
    pub overlay_version: u32,
}

impl SessionMeta {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}
