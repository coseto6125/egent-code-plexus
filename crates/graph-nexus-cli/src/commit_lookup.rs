use graph_nexus_core::registry::CommitDirName;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::SystemTime;

/// In-memory `sha → dirname` map built by scanning a `<repo>/commits/` dir.
/// Built lazily once per CLI invocation; `find()` is O(1).
///
/// Unparseable dir names (garbage / partial `.building` / `.stale` leftovers)
/// are skipped, not surfaced — they are recovery debris, not query targets.
pub struct CommitIndex {
    by_sha: HashMap<[u8; 20], String>,
}

/// Process-level cache for `scan_cached`. Keyed by commits dir path, valued by
/// (mtime at scan time, Arc-wrapped index). mtime mismatch ⇒ rescan. Long-
/// lived MCP servers hit this on every `Engine::open` / classify; the cache
/// turns N readdir per query into N stat per query.
///
/// RwLock: hot path is the read-side (cache hit), only misses take the write
/// lock — concurrent queries don't serialize. Arc avoids cloning the inner
/// HashMap on every hit. Bounded by repo count seen in this process; not
/// LRU-evicted today because typical MCP / CLI usage tops out at <20 repos.
static SCAN_CACHE: OnceLock<RwLock<HashMap<PathBuf, (SystemTime, Arc<CommitIndex>)>>> =
    OnceLock::new();

impl CommitIndex {
    pub fn scan(commits_dir: &Path) -> io::Result<Self> {
        let mut by_sha = HashMap::new();
        let it = match std::fs::read_dir(commits_dir) {
            Ok(d) => d,
            // commits/ dir absent on first build for a new repo — empty index, not error
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self { by_sha }),
            Err(e) => return Err(e),
        };
        for entry in it.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // Skip in-flight builds and stale dirs reserved by promotion Case B
            if name.ends_with(".building") || name.contains(".stale") {
                continue;
            }
            let Ok(parsed) = CommitDirName::parse(&name) else {
                continue;
            };
            by_sha.insert(parsed.sha, name);
        }
        Ok(Self { by_sha })
    }

    /// `scan` + process-level cache keyed on `commits_dir` mtime. Atomic
    /// commit-dir publish (rename of `<dirname>.building/` → `<dirname>/`)
    /// bumps parent `commits/` mtime, so a fresh scan happens on the very
    /// next call after a publish. Cache miss / unavailable mtime falls
    /// through to plain `scan`. Used by classify in hot-path query setup.
    ///
    /// Returns `Arc<Self>` so callers share the cached map without cloning.
    /// Poisoned lock is recovered via `into_inner()` per Rust stdlib idiom:
    /// poison just means a previous holder panicked, the data itself is
    /// fine — silently swallowing would leave the cache permanently dark.
    pub fn scan_cached(commits_dir: &Path) -> io::Result<Arc<Self>> {
        let Some(mtime) = std::fs::metadata(commits_dir)
            .ok()
            .and_then(|m| m.modified().ok())
        else {
            return Ok(Arc::new(Self::scan(commits_dir)?));
        };
        let cache = SCAN_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
        {
            let map = cache.read().unwrap_or_else(|e| e.into_inner());
            if let Some((cached_mt, idx)) = map.get(commits_dir) {
                if *cached_mt == mtime {
                    return Ok(Arc::clone(idx));
                }
            }
        }
        let fresh = Arc::new(Self::scan(commits_dir)?);
        let mut map = cache.write().unwrap_or_else(|e| e.into_inner());
        map.insert(commits_dir.to_path_buf(), (mtime, Arc::clone(&fresh)));
        Ok(fresh)
    }

    pub fn find(&self, sha: &[u8; 20]) -> Option<&str> {
        self.by_sha.get(sha).map(|s| s.as_str())
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.by_sha.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_sha.is_empty()
    }
}

/// Find the commit dir under `commits_dir` whose `graph.bin` has the
/// most recent mtime. Used as fallback when SHA-keyed lookup misses
/// (e.g. branch not yet indexed; pick most-recently-built as best guess).
///
/// Skips `.building` / `.stale-*` dirs (belt-and-suspenders — `scan()`
/// already filters these, but this operates on the raw dir listing).
pub fn find_latest_by_mtime(commits_dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(commits_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            !s.ends_with(".building") && !s.contains(".stale")
        })
        .filter_map(|e| {
            let graph_bin = e.path().join("graph.bin");
            let mtime = std::fs::metadata(&graph_bin).ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}
