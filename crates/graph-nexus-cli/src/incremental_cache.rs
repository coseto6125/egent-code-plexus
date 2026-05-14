//! Incremental analysis cache — per-file `LocalGraph` memoization keyed by
//! `SHA256(source_bytes)` so subsequent `gnx analyze` runs can skip the
//! tree-sitter parse for files that haven't changed.
//!
//! ## Correctness model
//!
//! Cache entries are reused **only** when both:
//! 1. The recorded `parser_fingerprint` matches the current binary's
//!    `GRAPH_NEXUS_PARSER_FINGERPRINT` (set at build time by
//!    `graph-nexus-analyzer/build.rs` from a SHA256 over every parser.rs,
//!    queries.scm, and shared helper file).
//! 2. The recorded `content_hash` matches a fresh `SHA256` of the file's
//!    current bytes on disk.
//!
//! Either mismatch → the entry is treated as missing and the file is
//! re-parsed normally. **The Build phase (Pass 1-4) always runs end-to-end
//! over the merged set**, so cross-file dependencies (renames, deleted
//! exports) are resolved correctly even when some `LocalGraph`s came from
//! cache. The cache only memoizes the deterministic tree-sitter parse +
//! query-capture stage of a single file.
//!
//! ## File layout
//!
//! `<.gitnexus-rs>/incremental_cache.bin` — a single rkyv-serialized
//! [`CacheFile`] struct containing a `schema_version`, a snapshot of the
//! `parser_fingerprint` at write time, and the per-file payload. rkyv
//! structural validation on load doubles as a corruption check; if a
//! cosmic-ray bit flip slips past it, the worst case is a forged
//! `content_hash` collision, which is statistically impossible.
//!
//! ## Failure mode
//!
//! Every error path here is best-effort. A missing/corrupt/incompatible
//! cache file degrades silently to a full re-parse — the graph.bin
//! artifact is the source of truth and is rebuilt from scratch every run
//! regardless of cache state.

use graph_nexus_core::analyzer::types::LocalGraph;
use rkyv::{Archive, Deserialize, Serialize};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Bump when `CacheFile` / `CachedEntry` / `LocalGraph` (or any transitively
/// archived type) gains/removes/reorders a field. rkyv archived layout is
/// not forward-compatible across struct changes.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// Compile-time fingerprint over every parser.rs / queries.scm / shared
/// helper file in `graph-nexus-analyzer`. Re-exported from the analyzer
/// crate (not read here via `env!()` directly because that macro resolves
/// in the caller's crate context, and the env var is set by analyzer's
/// own `build.rs`).
pub const PARSER_FINGERPRINT: &str = graph_nexus_analyzer::PARSER_FINGERPRINT;

/// A single file's memoized parse result.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct CachedEntry {
    /// Repo-relative path. Matches `LocalGraph.file_path` so lookups can
    /// key on the same value the pipeline already produces.
    #[rkyv(with = rkyv::with::AsString)]
    pub file_path: PathBuf,

    /// `SHA256(source_bytes)` recorded at the time of parse.
    pub content_hash: [u8; 32],

    /// The full parse output, re-emitted verbatim on cache hit.
    pub local_graph: LocalGraph,
}

/// On-disk root struct. Stored as a single rkyv blob — `schema_version`
/// together with `parser_fingerprint` form the "fingerprint" half of the
/// fingerprint-plus-atomic-swap consistency pattern.
#[derive(Archive, Deserialize, Serialize, Debug)]
#[rkyv(derive(Debug))]
pub struct CacheFile {
    pub schema_version: u32,
    /// Hex string of the SHA256 emitted by `build.rs`. Stored as `String`
    /// (not `[u8; 32]`) so future versions can change the digest algorithm
    /// without a schema bump.
    pub parser_fingerprint: String,
    pub entries: Vec<CachedEntry>,
}

/// Index built from a loaded cache for O(1) `(file_path) → entry` lookup
/// during `pipeline.analyze`. Owns the deserialized cache by value (not a
/// mmap view) because rkyv 0.8's `&ArchivedLocalGraph → LocalGraph`
/// hop on every hit is cheaper than re-doing the parse, but we'd need a
/// self-referential struct to mmap-zero-copy the whole thing — defer
/// that micro-optimization until benchmarks show it matters.
pub struct CacheIndex {
    /// `file_path` → `(content_hash, local_graph)`. `FxHashMap` here because
    /// the same hot-path argument applies as for `StringPool` /
    /// `SymbolTable` (short string keys, no HashDoS surface).
    by_path: FxHashMap<PathBuf, ([u8; 32], LocalGraph)>,
}

impl CacheIndex {
    /// O(1) lookup. Returns `Some(local_graph)` iff the file is in the
    /// cache **and** its recorded `content_hash` matches the caller's.
    /// Caller is responsible for hashing the current bytes — this keeps
    /// the cache pure (no disk I/O during lookup).
    pub fn get(&self, file_path: &Path, content_hash: &[u8; 32]) -> Option<LocalGraph> {
        let (cached_hash, lg) = self.by_path.get(file_path)?;
        if cached_hash == content_hash {
            Some(lg.clone())
        } else {
            None
        }
    }

    /// Diagnostic: total number of entries the index covers (not the
    /// number of cache hits — that's tracked at the call site).
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    /// Clippy convention helper alongside `len`. Currently unused in the
    /// live code path (callers check `len() > 0` directly via the
    /// `cache_count_pre` field surfaced for logging) but kept so a future
    /// `Iterator::is_empty`-style consumer doesn't have to add it.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }
}

/// SHA256 of the given byte slice, returned as a fixed-size array so it
/// can sit in `LocalGraph.content_hash` without an extra alloc.
///
/// Currently unused outside tests — the live `pipeline.analyze` path
/// recomputes the hash inline (already had a hasher in scope and avoids
/// the extra fn call). Kept for tests + future callers that want a
/// standalone hash helper.
#[allow(dead_code)]
pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Best-effort cache load.
///
/// Returns `None` on any failure mode — missing file, I/O error, rkyv
/// validation error, schema mismatch, or `parser_fingerprint` mismatch.
/// The caller treats `None` as "no cache available, fall back to full
/// parse"; nothing in the analyze pipeline depends on cache success.
///
/// On hit (`Some`), every entry in the returned index is guaranteed to
/// match both the current schema and the current parser binary — so
/// only the per-file `content_hash` still needs to be checked at lookup.
pub fn load_cache(path: &Path) -> Option<CacheIndex> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return None, // missing or unreadable — first run / clean state
    };

    // rkyv structural validation catches truncation, corruption, and
    // schema-shape changes. The two explicit equality checks below catch
    // the "shape is right but contents are stale" case.
    let archived = match rkyv::access::<ArchivedCacheFile, rkyv::rancor::Error>(&bytes) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("incremental cache structurally invalid, discarding: {e}");
            return None;
        }
    };

    if archived.schema_version.to_native() != CACHE_SCHEMA_VERSION {
        tracing::info!(
            "incremental cache schema mismatch ({} vs {CACHE_SCHEMA_VERSION}); discarding",
            archived.schema_version.to_native()
        );
        return None;
    }
    if archived.parser_fingerprint.as_ref() != PARSER_FINGERPRINT {
        tracing::info!("incremental cache parser fingerprint changed; discarding");
        return None;
    }

    // Deserialize entries into owned form for the index. We could keep
    // the mmap'd archived view to skip this hop, but it requires a
    // self-referential struct (mmap + ArchivedCacheFile bound to it);
    // sticking to owned LocalGraphs keeps the lifetime story simple.
    let cache: CacheFile = match rkyv::deserialize::<CacheFile, rkyv::rancor::Error>(archived) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("incremental cache deserialize failed, discarding: {e}");
            return None;
        }
    };

    let mut by_path = FxHashMap::default();
    by_path.reserve(cache.entries.len());
    for e in cache.entries {
        by_path.insert(e.file_path, (e.content_hash, e.local_graph));
    }
    Some(CacheIndex { by_path })
}

/// Best-effort cache save. Writes the next-run cache from the final set
/// of `LocalGraph`s + their `content_hash`es. Errors are logged but never
/// propagated — a save failure can never break the analyze run.
///
/// Uses `atomic_write_bytes` (write to `<path>.tmp` + rename) so a Ctrl-C
/// mid-write leaves the previous cache intact, never a half-truncated one.
pub fn save_cache(path: &Path, entries: Vec<CachedEntry>) {
    let cache = CacheFile {
        schema_version: CACHE_SCHEMA_VERSION,
        parser_fingerprint: PARSER_FINGERPRINT.to_string(),
        entries,
    };

    let bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(&cache) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("incremental cache serialize failed, skipping save: {e}");
            return;
        }
    };

    if let Some(parent) = path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("incremental cache parent dir create failed: {e}");
                return;
            }
        }
    }

    if let Err(e) = graph_nexus_core::registry::atomic_write_bytes(path, &bytes) {
        tracing::warn!("incremental cache write failed, skipping: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::analyzer::types::{LocalGraph, RawNode};
    use graph_nexus_core::graph::NodeKind;
    use tempfile::TempDir;

    fn fake_local_graph(file_name: &str, content_hash: [u8; 32]) -> LocalGraph {
        LocalGraph {
            file_path: PathBuf::from(file_name),
            content_hash,
            nodes: vec![RawNode {
                name: "foo".to_string(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        }
    }

    #[test]
    fn round_trip_save_and_load_recovers_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.bin");
        let hash = hash_bytes(b"pub fn foo() {}");

        let entries = vec![CachedEntry {
            file_path: PathBuf::from("src/foo.rs"),
            content_hash: hash,
            local_graph: fake_local_graph("src/foo.rs", hash),
        }];
        save_cache(&path, entries);

        let loaded = load_cache(&path).expect("cache must load");
        assert_eq!(loaded.len(), 1);

        let recovered = loaded
            .get(Path::new("src/foo.rs"), &hash)
            .expect("entry must be retrievable with matching hash");
        assert_eq!(recovered.nodes.len(), 1);
        assert_eq!(recovered.nodes[0].name, "foo");
    }

    #[test]
    fn content_hash_mismatch_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.bin");
        let saved_hash = hash_bytes(b"v1");
        let queried_hash = hash_bytes(b"v2");

        save_cache(
            &path,
            vec![CachedEntry {
                file_path: PathBuf::from("a.rs"),
                content_hash: saved_hash,
                local_graph: fake_local_graph("a.rs", saved_hash),
            }],
        );

        let loaded = load_cache(&path).unwrap();
        assert!(loaded.get(Path::new("a.rs"), &queried_hash).is_none());
    }

    #[test]
    fn missing_cache_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.bin");
        assert!(load_cache(&path).is_none());
    }

    #[test]
    fn corrupt_cache_file_returns_none_not_panic() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.bin");
        std::fs::write(&path, b"not valid rkyv bytes").unwrap();
        assert!(load_cache(&path).is_none());
    }
}
