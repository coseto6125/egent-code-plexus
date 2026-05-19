//! Read/write `contracts.rkyv` + `meta.json`. Atomic rename pattern
//! mirrors `cgn_core::registry::io::atomic_write_json`.

use crate::commands::group::types::{ArchivedContractRegistry, ContractRegistry};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const CONTRACTS_FILE: &str = "contracts.rkyv";
pub const META_FILE: &str = "meta.json";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GroupMeta {
    pub version: u32,
    pub generated_at: String,
    pub repo_snapshots: BTreeMap<String, RepoSnapshot>,
    pub missing_repos: Vec<String>,
    /// Which config source was used for this sync run.
    /// `"default"` = built-in defaults; `"file"` = loaded from `~/.gnx/config.toml`.
    pub config_source: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RepoSnapshot {
    pub indexed_at: String,
    pub last_commit: String,
}

pub fn group_dir(home_gnx: &Path, group_name: &str) -> PathBuf {
    home_gnx.join("groups").join(group_name)
}

pub fn write_contracts(group_dir: &Path, reg: &ContractRegistry) -> io::Result<()> {
    fs::create_dir_all(group_dir)?;
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(reg)
        .map_err(|e| io::Error::other(format!("rkyv: {e:?}")))?;
    let path = group_dir.join(CONTRACTS_FILE);
    cgn_core::registry::atomic_write_bytes(&path, &bytes)
}

pub fn read_contracts(group_dir: &Path) -> io::Result<ContractRegistry> {
    let path = group_dir.join(CONTRACTS_FILE);
    if !path.exists() {
        return Ok(ContractRegistry {
            version: 1,
            contracts: vec![],
            cross_links: vec![],
            unmatched: vec![],
        });
    }
    let bytes = fs::read(&path)?;
    rkyv::from_bytes::<ContractRegistry, rkyv::rancor::Error>(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("rkyv: {e:?}")))
}

pub fn write_meta(group_dir: &Path, meta: &GroupMeta) -> io::Result<()> {
    fs::create_dir_all(group_dir)?;
    cgn_core::registry::atomic_write_json(&group_dir.join(META_FILE), meta)
}

/// Hot-path mmap handle. Owns the `Mmap` and exposes zero-copy access via
/// `archived()`. `read_contracts` is for write-then-read flows (tests, debug
/// listing) only; this type is for the <30 ms per-query hot path.
pub struct ContractsMmap(Mmap);

impl ContractsMmap {
    pub fn archived(&self) -> io::Result<&ArchivedContractRegistry> {
        rkyv::access::<ArchivedContractRegistry, rkyv::rancor::Error>(&self.0)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("rkyv: {e:?}")))
    }
}

/// Open `contracts.rkyv` as a memory-mapped zero-copy handle. Hot-path mmap;
/// `read_contracts` is for write-then-read flows only.
pub fn read_contracts_archived(group_dir: &Path) -> io::Result<ContractsMmap> {
    let path = group_dir.join(CONTRACTS_FILE);
    let file = fs::File::open(&path)?;
    // SAFETY: the file is not modified while this mapping is alive; writers
    // use atomic rename so any new version lands at a fresh inode.
    let mmap = unsafe { Mmap::map(&file)? };
    Ok(ContractsMmap(mmap))
}

pub fn read_meta(group_dir: &Path) -> io::Result<GroupMeta> {
    let path = group_dir.join(META_FILE);
    let bytes = fs::read(&path)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
