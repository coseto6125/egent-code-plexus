//! Per-file persistent parse cache.
//!
//! Stores tree-sitter `LocalGraph` blobs at
//! `<home_gnx>/<repo>/parse_cache/<fp>/<content_hash>.rkyv`, where `<fp>` is
//! an 8-hex-char digest of [`BUILDER_FINGERPRINT`] — scoping each entry
//! to one binary build so an upgrade can't replay stale parser output
//! against a fresh reader. The pipeline's per-file `cache_lookup` hook
//! short-circuits to a cached graph when the file's `xxh3_64(content)`
//! matches an existing entry; misses fall through to the regular
//! tree-sitter parse and are written back here for next time.
//!
//! Cache scope is per-repo (caller picks the root), per-fingerprint.
//! Cross-repo content collisions are impossible because the hash is over
//! file bytes — same bytes yield the same graph regardless of where they
//! live. The fingerprint subdir is the only invalidation lever; LRU /
//! quota / orphan sweep belong to a separate GC pass.

use crate::repo_identity::short_hash_hex8;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::registry::{atomic_write_bytes_no_fsync, BUILDER_FINGERPRINT};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// First 8 hex chars of a stable xxh3_64 digest of `BUILDER_FINGERPRINT` —
/// short, filesystem-safe, stable for the life of the process. Memoised
/// because `BUILDER_FINGERPRINT` is a compile-time constant.
fn fingerprint_dir_name() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| short_hash_hex8(BUILDER_FINGERPRINT.as_bytes()))
}

pub struct ParseCache {
    root: PathBuf,
}

impl ParseCache {
    /// Open (and create on demand) the cache at
    /// `<repo_root>/parse_cache/<fp>/`. `repo_root` should be the per-repo
    /// dir under `~/.gnx/` (e.g. `~/.gnx/myrepo__abc123`).
    pub fn open(repo_root: &Path) -> std::io::Result<Self> {
        let root = repo_root.join("parse_cache").join(fingerprint_dir_name());
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Filesystem location for a given content hash. Exposed for tests
    /// that need to seed corrupted blobs or inspect on-disk layout.
    pub fn path_for(&self, content_hash: &[u8; 8]) -> PathBuf {
        self.root
            .join(format!("{:016x}.rkyv", u64::from_le_bytes(*content_hash)))
    }

    /// Read a cached `LocalGraph` keyed by its content hash. Returns
    /// `None` on miss, corruption, or read error — callers always have
    /// a safe fall-through to the regular parse path. Corrupt entries
    /// are deleted so the next `put` for the same key writes clean
    /// (without this, a single bad blob poisons that key forever).
    pub fn get(&self, content_hash: &[u8; 8]) -> Option<LocalGraph> {
        let path = self.path_for(content_hash);
        let bytes = std::fs::read(&path).ok()?;
        match rkyv::from_bytes::<LocalGraph, rkyv::rancor::Error>(&bytes) {
            Ok(g) => Some(g),
            Err(e) => {
                tracing::warn!(
                    "parse_cache: dropping corrupt entry {}: {}",
                    path.display(),
                    e
                );
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// Persist a freshly parsed `LocalGraph`. Uses `atomic_write_bytes_no_fsync`
    /// (tmp + rename, no `sync_all`): parse-cache blobs are content-addressable
    /// + fully regeneratable from source, so a torn write on crash is
    /// recoverable (the corrupt-entry guard in `get()` deletes and the next
    /// miss reparses). Skipping the fsync converts a per-file ~2ms sync syscall
    /// into a kernel-deferred write — on cold-index over 14k files this drops
    /// the cache-write phase from ~30s to <1s.
    pub fn put(&self, graph: &LocalGraph) -> std::io::Result<()> {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(graph)
            .map_err(std::io::Error::other)?;
        atomic_write_bytes_no_fsync(&self.path_for(&graph.content_hash), &bytes)
    }
}
