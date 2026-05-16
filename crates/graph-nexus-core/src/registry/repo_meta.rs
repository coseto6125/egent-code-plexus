use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoMeta {
    pub version: u32,
    pub common_dir: String,
    pub remote_url: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub known_refs: BTreeMap<String, String>,
    pub last_built_sha: Option<String>,
    pub total_size_bytes: u64,
    pub last_touched: String,
}

impl RepoMeta {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}
