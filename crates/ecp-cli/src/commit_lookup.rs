use ecp_core::registry::{CommitDirName, Generation};
use rustc_hash::FxHashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::SystemTime;

type CommitIndexCache = RwLock<FxHashMap<PathBuf, (SystemTime, Arc<CommitIndex>)>>;

/// In-memory `sha → dirname` map built by scanning a `<repo>/commits/` dir.
/// Built lazily once per CLI invocation; `find()` is O(1).
///
/// Unparseable dir names (garbage / partial `.building` / `.stale` leftovers)
/// are skipped, not surfaced — they are recovery debris, not query targets.
pub struct CommitIndex {
    by_sha: FxHashMap<[u8; 20], String>,
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
static SCAN_CACHE: OnceLock<CommitIndexCache> = OnceLock::new();

/// Sort key for same-SHA tie-breaking. Primary axis is `generation` (a
/// deterministic 3-tuple the producer encodes into the dir name), with mtime
/// retained only as a tertiary fallback. The pre-FU-045 behaviour was
/// mtime-only, which raced against ext4 / APFS mtime resolution + non-sorted
/// `read_dir` order — equal mtimes silently picked whichever dir the OS
/// iterated first.
///
/// `Ord` derived on the struct gives lexicographic `(generation, mtime)`:
/// - `None < Some(_)`, so a base dir always loses to any generation dir.
/// - Between two `Some(_)` generations, `(timestamp_ms, pid, counter)` wins
///   in lex order.
/// - `mtime` only differentiates when generations are equal (which the
///   producer guarantees never happens for distinct builds — the `counter`
///   field is monotonic per process, and `pid + timestamp_ms` covers
///   cross-process collisions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Freshness {
    generation: Option<Generation>,
    mtime: SystemTime,
}

impl CommitIndex {
    pub fn scan(commits_dir: &Path) -> io::Result<Self> {
        let mut candidates: FxHashMap<[u8; 20], (String, Freshness)> = FxHashMap::default();
        let it = match std::fs::read_dir(commits_dir) {
            Ok(d) => d,
            // commits/ dir absent on first build for a new repo — empty index, not error
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    by_sha: FxHashMap::default(),
                });
            }
            Err(e) => return Err(e),
        };
        for entry in it.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // Skip in-flight builds and retired/stale recovery debris.
            if is_recovery_dir_name(&name) {
                continue;
            }
            let Ok(parsed) = CommitDirName::parse(&name) else {
                continue;
            };
            let freshness = Freshness {
                generation: parsed.generation,
                mtime: commit_dir_mtime(&entry.path()),
            };
            match candidates.get(&parsed.sha) {
                Some((_existing, existing)) if *existing >= freshness => {}
                _ => {
                    candidates.insert(parsed.sha, (name, freshness));
                }
            }
        }
        Ok(Self {
            by_sha: candidates
                .into_iter()
                .map(|(sha, (name, _freshness))| (sha, name))
                .collect(),
        })
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
        let cache = SCAN_CACHE.get_or_init(|| RwLock::new(FxHashMap::default()));
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

/// Read the freshest mtime available on a commit dir: prefer `meta.json`,
/// fall back to `graph.bin`, finally the dir itself. Returns
/// `SystemTime::UNIX_EPOCH` if every stat fails — that loses every
/// same-SHA tie under the [`Freshness`] order, which is the safe default
/// for a half-broken / not-yet-published dir.
///
/// Only the *secondary* tie-breaker after the parsed [`Generation`] suffix —
/// generation tuples are the authoritative freshness signal post-FU-045.
fn commit_dir_mtime(path: &Path) -> SystemTime {
    path.join("meta.json")
        .metadata()
        .and_then(|m| m.modified())
        .or_else(|_| path.join("graph.bin").metadata().and_then(|m| m.modified()))
        .or_else(|_| path.metadata().and_then(|m| m.modified()))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn is_recovery_dir_name(name: &str) -> bool {
    let Some((_prefix, suffix)) = name.rsplit_once("__") else {
        return false;
    };
    suffix.ends_with(".building")
        || suffix.contains(".building.")
        || suffix.contains(".stale-")
        || suffix.ends_with(".dead")
        || suffix.contains(".dead.")
}

/// Collect every non-recovery commit dir under `commits_dir` that has a
/// `graph.bin`, paired with that file's mtime. Shared by the single-best and
/// all-by-mtime accessors so the readdir + recovery-dir filter live in one
/// place. Returns an empty vec when the dir is absent or unreadable.
fn graph_dirs_with_mtime(commits_dir: &Path) -> Vec<(SystemTime, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(commits_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            !is_recovery_dir_name(&s)
        })
        .filter_map(|e| {
            let graph_bin = e.path().join("graph.bin");
            let mtime = std::fs::metadata(&graph_bin).ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect()
}

/// Find the commit dir under `commits_dir` whose `graph.bin` has the
/// most recent mtime. Used as fallback when SHA-keyed lookup misses
/// (e.g. branch not yet indexed; pick most-recently-built as best guess).
///
/// Skips `.building` / `.stale-*` dirs (belt-and-suspenders — `scan()`
/// already filters these, but this operates on the raw dir listing).
pub fn find_latest_by_mtime(commits_dir: &Path) -> Option<PathBuf> {
    graph_dirs_with_mtime(commits_dir)
        .into_iter()
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Like `find_latest_by_mtime`, but returns ALL candidate commit dirs sorted
/// newest-first. Used by warm-attach to try the next-best sibling when the
/// single newest one fails a downstream gate (compatibility / commit distance)
/// — picking only the newest and giving up there leaves a usable sibling
/// unused on disk.
pub fn find_all_by_mtime_desc(commits_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = graph_dirs_with_mtime(commits_dir);
    dirs.sort_unstable_by_key(|(t, _)| std::cmp::Reverse(*t));
    dirs.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create `<commits>/<name>/graph.bin` with the given mtime offset (secs
    /// after the UNIX epoch) so ordering is deterministic regardless of the
    /// host filesystem's mtime granularity.
    fn published(commits: &Path, name: &str, mtime_secs: u64) -> PathBuf {
        let dir = commits.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let graph_bin = dir.join("graph.bin");
        std::fs::write(&graph_bin, b"x").unwrap();
        let t = filetime::FileTime::from_unix_time(mtime_secs as i64, 0);
        filetime::set_file_mtime(&graph_bin, t).unwrap();
        dir
    }

    #[test]
    fn all_by_mtime_desc_is_newest_first_and_latest_agrees() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        let oldest = published(commits, "commit__aa", 1_000);
        let middle = published(commits, "commit__bb", 2_000);
        let newest = published(commits, "commit__cc", 3_000);

        let all = find_all_by_mtime_desc(commits);
        assert_eq!(all, vec![newest.clone(), middle, oldest]);
        assert_eq!(find_latest_by_mtime(commits), Some(newest));
    }

    #[test]
    fn recovery_dirs_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        let good = published(commits, "commit__aa", 1_000);
        // Newest by mtime, but a recovery dir → must not surface.
        published(commits, "commit__bb.building", 9_000);
        published(commits, "commit__cc.dead", 9_000);

        assert_eq!(find_all_by_mtime_desc(commits), vec![good.clone()]);
        assert_eq!(find_latest_by_mtime(commits), Some(good));
    }

    /// A dir without a `graph.bin` is not a usable candidate.
    #[test]
    fn dirs_without_graph_bin_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        std::fs::create_dir_all(commits.join("commit__nobin")).unwrap();
        let good = published(commits, "commit__aa", 1_000);

        assert_eq!(find_all_by_mtime_desc(commits), vec![good]);
    }

    #[test]
    fn missing_dir_yields_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("does-not-exist");
        assert!(find_all_by_mtime_desc(&absent).is_empty());
        assert_eq!(find_latest_by_mtime(&absent), None);
    }
}
