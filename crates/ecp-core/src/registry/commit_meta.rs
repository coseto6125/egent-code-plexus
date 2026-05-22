use crate::registry::dirname::SourceType;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

/// String baked into every `CommitBuildMeta` at build time. The fast path
/// in `build_l2` only reuses a cached L2 entry when its persisted
/// fingerprint matches the running binary's — upgrading `ecp` (or bumping
/// the `+schema<N>` suffix below) invalidates older entries automatically,
/// preventing a stale parser from feeding mismatched bytes back to a new
/// reader.
///
/// Bump the `+schema<N>` literal whenever `graph.bin`, `CommitBuildMeta`,
/// or any persisted L2 artefact changes shape in a way pre-bump binaries
/// can't read back.
pub const BUILDER_FINGERPRINT: &str = concat!("v", env!("CARGO_PKG_VERSION"), "+schema1");

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
    /// Fingerprint of the binary that wrote this entry. `None` on meta
    /// files written by pre-fingerprint binaries — treated as a fast-path
    /// miss so the next run rewrites with the current fingerprint.
    #[serde(default)]
    pub builder_fingerprint: Option<String>,
    /// Short SHA of the ecp binary that wrote this entry. `None` on meta
    /// files written by pre-SHA binaries.
    #[serde(default)]
    pub binary_commit_sha: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn base_meta() -> CommitBuildMeta {
        CommitBuildMeta {
            version: 1,
            sha: "abc123def4567890abc123def4567890abc123de".into(),
            source_type: SourceType::Branch,
            source_id: Some("main".into()),
            built_from_worktree: "/work/repo".into(),
            built_at: "2026-05-23T00:00:00Z".into(),
            parent_sha: None,
            node_count: 10,
            embedding_status: EmbeddingStatus::None,
            refs_at_build: vec![],
            refs_seen_since: vec![],
            builder_fingerprint: None,
            binary_commit_sha: None,
        }
    }

    #[test]
    fn binary_commit_sha_roundtrip() {
        let meta = CommitBuildMeta {
            binary_commit_sha: Some("abc1234".into()),
            ..base_meta()
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: CommitBuildMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back, meta);
        assert_eq!(back.binary_commit_sha.as_deref(), Some("abc1234"));
    }

    #[test]
    fn binary_commit_sha_backcompat_missing_field() {
        // JSON written by a pre-SHA binary omits the field entirely.
        let json = r#"{
            "version": 1,
            "sha": "abc123def4567890abc123def4567890abc123de",
            "source_type": "Branch",
            "source_id": "main",
            "built_from_worktree": "/work/repo",
            "built_at": "2026-05-23T00:00:00Z",
            "parent_sha": null,
            "node_count": 10,
            "embedding_status": "None",
            "refs_at_build": [],
            "refs_seen_since": []
        }"#;
        let meta: CommitBuildMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.binary_commit_sha, None);
    }
}
