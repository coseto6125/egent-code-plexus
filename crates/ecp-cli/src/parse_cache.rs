//! Per-file persistent parse cache.
//!
//! Stores tree-sitter `LocalGraph` blobs at
//! `<home_ecp>/<repo>/parse_cache/<fp>/<path_hash><content_hash>.rkyv`, where
//! `<fp>` is an 8-hex-char digest of [`BUILDER_FINGERPRINT`] — scoping each
//! entry to one binary build so an upgrade can't replay stale parser output
//! against a fresh reader. The pipeline's per-file `cache_lookup` hook
//! short-circuits to a cached graph when both the file's path and its
//! `xxh3_64(content)` match an existing entry; misses fall through to the
//! regular tree-sitter parse and are written back here for next time.
//!
//! **The path is half the key, and it has to be.** A `LocalGraph` carries the
//! `file_path` it was parsed from, and the builder reads that field back to
//! decide which File node the symbols belong to. Keying on content alone let
//! two byte-identical files share one entry: the second file's symbols came
//! back wearing the first file's path, collided on uid, and were tombstoned
//! to empty names — so the file vanished from the graph while `ecp find`
//! reported success. Byte-identical files are ordinary (empty `__init__.py`,
//! generated stubs, duplicated configs), so this was not a corner case.
//!
//! Cache scope is per-repo (caller picks the root), per-fingerprint, per-path.
//! The fingerprint subdir is the only invalidation lever; LRU / quota /
//! orphan sweep belong to a separate GC pass.

use crate::repo_identity::short_hash_hex8;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::registry::{atomic_write_bytes_no_fsync, BUILDER_FINGERPRINT};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// First 8 hex chars of a stable xxh3_64 digest of `BUILDER_FINGERPRINT` —
/// short, filesystem-safe, stable for the life of the process. Memoised
/// because `BUILDER_FINGERPRINT` is a compile-time constant.
fn fingerprint_dir_name() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| short_hash_hex8(BUILDER_FINGERPRINT.as_bytes()))
}

/// Stable digest of a repo-relative path. Separators are normalised to `/`
/// only when the platform uses something else, so the common case hashes the
/// borrowed bytes with no allocation.
fn path_key(rel_path: &Path) -> u64 {
    let raw = rel_path.to_string_lossy();
    match raw.contains('\\') {
        true => xxhash_rust::xxh3::xxh3_64(raw.replace('\\', "/").as_bytes()),
        false => xxhash_rust::xxh3::xxh3_64(raw.as_bytes()),
    }
}

pub struct ParseCache {
    root: PathBuf,
}

impl ParseCache {
    /// Open (and create on demand) the cache at
    /// `<repo_root>/parse_cache/<fp>/`. `repo_root` should be the per-repo
    /// dir under `~/.ecp/` (e.g. `~/.ecp/myrepo__abc123`).
    pub fn open(repo_root: &Path) -> std::io::Result<Self> {
        let root = repo_root.join("parse_cache").join(fingerprint_dir_name());
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Filesystem location for one `(rel_path, content_hash)` pair. Exposed
    /// for tests that need to seed corrupted blobs or inspect on-disk layout.
    ///
    /// The two halves stay separate in the name rather than being hashed
    /// together, so an entry on disk can be traced back to its file by
    /// hashing a candidate path.
    pub fn path_for(&self, rel_path: &Path, content_hash: &[u8; 8]) -> PathBuf {
        self.root.join(format!(
            "{:016x}{:016x}.rkyv",
            path_key(rel_path),
            u64::from_le_bytes(*content_hash)
        ))
    }

    /// Read a cached `LocalGraph` for one `(rel_path, content_hash)`. Returns
    /// `None` on miss, corruption, or read error — callers always have
    /// a safe fall-through to the regular parse path. Corrupt entries
    /// are deleted so the next `put` for the same key writes clean
    /// (without this, a single bad blob poisons that key forever).
    pub fn get(&self, rel_path: &Path, content_hash: &[u8; 8]) -> Option<LocalGraph> {
        let path = self.path_for(rel_path, content_hash);
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
    /// and fully regeneratable from source, so a torn write on crash is
    /// recoverable (the corrupt-entry guard in `get()` deletes and the next
    /// miss reparses). Skipping the fsync converts a per-file ~2ms sync syscall
    /// into a kernel-deferred write — on cold-index over 14k files this drops
    /// the cache-write phase from ~30s to <1s.
    ///
    /// Retained for integration tests (`tests/parse_cache.rs`). Production
    /// callers use the inlined serialize-then-background-write path in
    /// `commands::admin::index` (CI-A) which avoids holding the global rayon
    /// pool while disk writes drain. `#[allow(dead_code)]` because the lib
    /// target has no internal caller; tests are a separate compilation target
    /// so the dead-code lint can't see them.
    #[allow(dead_code)]
    pub fn put(&self, graph: &LocalGraph) -> std::io::Result<()> {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(graph).map_err(std::io::Error::other)?;
        atomic_write_bytes_no_fsync(
            &self.path_for(&graph.file_path, &graph.content_hash),
            &bytes,
        )
    }
}
