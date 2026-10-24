use ecp_core::registry::CommitDirName;
use rustc_hash::FxHashMap;
use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// In-memory `sha → dirname` map built by scanning a `<repo>/commits/` dir.
/// Built lazily once per CLI invocation; `find()` is O(1).
///
/// Unparseable dir names (garbage / partial `.building` / `.stale` leftovers)
/// are skipped, not surfaced — they are recovery debris, not query targets.
pub struct CommitIndex {
    by_sha: FxHashMap<[u8; 20], String>,
}

/// A cached scan is reused only while it is younger than this, measured on the
/// monotonic clock. Bounds how long a publish that left the dir mtime
/// unchanged (two renames in one coarse timestamp tick, or a wall clock that
/// stepped back) stays invisible to this process, whatever the wall clock does.
const SCAN_CACHE_MAX_AGE: Duration = Duration::from_secs(2);

/// A scan is cached only when the dir mtime lies at least this far before the
/// scan started. A younger mtime can still be shared by a publish in the same
/// tick: Linux stamps directories from a coarse clock, HFS+ keeps 1 s, FAT 2 s.
const SCAN_CACHE_SETTLE: Duration = Duration::from_secs(2);

struct CachedScan {
    dir_mtime: SystemTime,
    scanned_at: Instant,
    index: Arc<CommitIndex>,
}

/// One commits dir's cache entry. `epoch` moves on every `invalidate`; a scan
/// that began under an older epoch is not stored, so a scan that started
/// before a publish cannot put the pre-publish answer back after it.
#[derive(Default)]
struct CacheSlot {
    epoch: u64,
    scan: Option<CachedScan>,
}

type CommitIndexCache = RwLock<FxHashMap<PathBuf, CacheSlot>>;

/// Process-level cache for `scan_cached`, keyed by commits dir path. Long-
/// lived MCP servers hit this on every `Engine::open` / classify; the cache
/// turns N readdir per query into N stat per query.
///
/// RwLock: hot path is the read-side (cache hit), only misses take the write
/// lock — concurrent queries don't serialize. Arc avoids cloning the inner
/// HashMap on every hit. Bounded by repo count seen in this process; not
/// LRU-evicted today because typical MCP / CLI usage tops out at <20 repos.
static SCAN_CACHE: OnceLock<CommitIndexCache> = OnceLock::new();

fn scan_cache() -> &'static CommitIndexCache {
    SCAN_CACHE.get_or_init(|| RwLock::new(FxHashMap::default()))
}

/// A published-name candidate in `commits/`: a directory whose name parses as
/// a commit dir and is not recovery debris.
#[derive(Debug, Clone)]
pub struct CommitDir {
    pub name: String,
    pub parsed: CommitDirName,
    /// `commits/<name>`.
    pub path: PathBuf,
    /// [`commit_dir_mtime`], read on the first equal-generation tie only: it
    /// costs a `stat`, and a cache with 130 commit dirs spent 1.3 ms of every
    /// query on stats that decided nothing.
    mtime: OnceCell<SystemTime>,
}

impl CommitDir {
    fn mtime(&self) -> SystemTime {
        *self.mtime.get_or_init(|| commit_dir_mtime(&self.path))
    }
}

/// The two staging shapes a build writes into `commits/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagingShape {
    /// `<base>.building`, written by every ecp before the SHA lock. Its
    /// builder holds `<base>.building/.build.lock` until just before its
    /// rename; a current binary never writes, reuses or removes such a dir.
    Legacy,
    /// `<base>.building.<pid>.<attempt>`: one build attempt, which exists only
    /// while its builder holds the SHA lock.
    PerAttempt { pid: u32, attempt: u64 },
}

#[derive(Debug, Clone)]
pub struct StagingDir {
    pub name: String,
    pub sha: [u8; 20],
    pub shape: StagingShape,
}

/// The newest write to an old-style staging dir: the dir itself, its
/// `meta.json` or its `graph.bin`. `None` when none of them can be read.
pub(crate) fn legacy_staging_last_write(dir: &Path) -> Option<SystemTime> {
    [
        dir.to_path_buf(),
        dir.join("meta.json"),
        dir.join("graph.bin"),
    ]
    .iter()
    .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
    .max()
}

/// The name of one build attempt's staging dir.
pub(crate) fn staging_dir_name(base: &str, pid: u32, attempt: u64) -> String {
    format!("{base}.building.{pid}.{attempt}")
}

/// Either staging shape, or `None`. A retired staging dir
/// (`….building.<pid>.<n>.dead.…`) is neither.
pub(crate) fn parse_staging_name(name: &str) -> Option<StagingDir> {
    let (base, rest) = name.rsplit_once(".building")?;
    let shape = if rest.is_empty() {
        StagingShape::Legacy
    } else {
        let (pid, attempt) = rest.strip_prefix('.')?.split_once('.')?;
        StagingShape::PerAttempt {
            pid: pid.parse().ok()?,
            attempt: attempt.parse().ok()?,
        }
    };
    let parsed = CommitDirName::parse(base).ok()?;
    Some(StagingDir {
        name: name.to_string(),
        sha: parsed.sha,
        shape,
    })
}

/// One read of `commits/`: the candidates, the staging dirs, and whether the
/// read saw every entry. The single reader of `commits/` for `CommitIndex`,
/// the builders and every gc sweep of it.
#[derive(Debug, Default)]
pub struct CommitDirListing {
    pub dirs: Vec<CommitDir>,
    /// Staging dirs of both shapes; never candidates.
    pub staging: Vec<StagingDir>,
    /// Dirs a retire renamed out of view (`crate::admin::gc::is_retired_dir`)
    /// whose delete has not finished.
    pub retired: Vec<String>,
    /// Directories that are neither commit dirs nor recovery debris. Only the
    /// cross-SHA `graph.bin` lookup reads them, as it did before this reader.
    pub unparsed: Vec<String>,
    /// False when an entry or its file type could not be read. Name choice
    /// and the gc generation sweep stop on an incomplete listing; readers
    /// answer from it, best effort.
    pub complete: bool,
}

impl CommitDirListing {
    /// `commits_dir` absent reads as an empty, complete listing.
    pub fn read(commits_dir: &Path) -> io::Result<Self> {
        let it = match std::fs::read_dir(commits_dir) {
            Ok(it) => it,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    complete: true,
                    ..Self::default()
                })
            }
            Err(e) => return Err(e),
        };
        Ok(Self::from_entries(
            commits_dir,
            it.filter_map(|entry| {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => return Some(Err(e)),
                };
                // A non-UTF-8 name is never one ecp wrote.
                let name = entry.file_name().into_string().ok()?;
                // `DirEntry::file_type` comes from the directory read itself on
                // Linux and Windows, and does not follow a symlink.
                Some(entry.file_type().map(|t| (name, t.is_dir())))
            }),
        ))
    }

    /// The listing of `(name, is_dir)` entries of `commits_dir`; an `Err`
    /// marks it incomplete.
    pub fn from_entries(
        commits_dir: &Path,
        entries: impl IntoIterator<Item = io::Result<(String, bool)>>,
    ) -> Self {
        let mut listing = Self {
            complete: true,
            ..Self::default()
        };
        for entry in entries {
            let Ok((name, is_dir)) = entry else {
                listing.complete = false;
                continue;
            };
            if !is_dir || name == LOCKS_DIR {
                continue;
            }
            if crate::admin::gc::is_retired_dir(&name) {
                listing.retired.push(name);
                continue;
            }
            if is_recovery_dir_name(&name) {
                if let Some(staging) = parse_staging_name(&name) {
                    listing.staging.push(staging);
                }
                continue;
            }
            match CommitDirName::parse(&name) {
                Ok(parsed) => listing.dirs.push(CommitDir {
                    path: commits_dir.join(&name),
                    name,
                    parsed,
                    mtime: OnceCell::new(),
                }),
                Err(_) => listing.unparsed.push(name),
            }
        }
        listing
    }

    pub fn same_sha<'a>(&'a self, sha: &[u8; 20]) -> impl Iterator<Item = &'a CommitDir> + 'a {
        let sha = *sha;
        self.dirs.iter().filter(move |d| d.parsed.sha == sha)
    }

    /// The current build of `sha`: the greatest under [`rank_cmp`].
    pub fn current(&self, sha: &[u8; 20]) -> Option<&CommitDir> {
        self.same_sha(sha).max_by(|a, b| rank_cmp(a, b))
    }
}

/// The one order on same-SHA dirs. Generation first (`None` below every
/// `Some`); on equal generations the newer [`commit_dir_mtime`]; then the
/// greater name. A builder holding the SHA lock picks a generation above every
/// existing one, so the mtime step decides only between dirs older binaries
/// wrote, as it did before, and the name step makes the order total, so the
/// reader and gc can never keep different dirs.
pub fn rank_cmp(a: &CommitDir, b: &CommitDir) -> Ordering {
    a.parsed
        .generation
        .cmp(&b.parsed.generation)
        .then_with(|| a.mtime().cmp(&b.mtime()))
        .then_with(|| a.name.cmp(&b.name))
}

/// The dir under `commits/` that holds the SHA locks. Every scan of
/// `commits/` skips it.
pub const LOCKS_DIR: &str = ".locks";

/// The lock every builder and gc of this binary takes for one SHA of one
/// repo: `commits/.locks/<xx>.lock`, `xx` the first byte of the SHA in
/// lowercase hex (the key is the parsed bytes, not text). One stripe covers
/// every SHA with that first byte, so a repo holds at most 256 lock files and
/// 256 owner sidecars however many SHAs it built; two SHAs of one stripe
/// build one after the other. A builder holds it from its last check for an
/// existing build through the rename and the fsync of `commits/`; gc holds
/// it to retire any dir of the SHA. Neither file is ever deleted: unlinking a
/// lock while a waiter holds a descriptor would let two holders in, one per
/// inode.
///
/// The guarantee ends at this binary. An ecp older than this lock neither
/// takes nor honours it: its builder can publish beside a new one, and its gc
/// can retire a previous generation while a new builder sits between its
/// rename and its fsync of `commits/`. Complete exclusion holds only once
/// every builder and gc on the machine runs this code.
pub fn sha_lock_path(commits_dir: &Path, sha: &[u8; 20]) -> PathBuf {
    commits_dir
        .join(LOCKS_DIR)
        .join(format!("{:02x}.lock", sha[0]))
}

/// Name the holder of `sha`'s stripe lock in its owner sidecar as
/// `<pid> <role> <sha40>`, so a waiter for another SHA of the stripe can tell
/// that this holder is not building its SHA.
pub(crate) fn record_sha_lock_owner(lock_path: &Path, role: &str, sha: &[u8; 20]) {
    ecp_core::registry::FileLock::record_owner(lock_path, &format!("{role} {}", hex::encode(sha)));
}

/// `sha`'s stripe lock without waiting, `.locks` created first, its holder
/// named as `role`. An `Err` of a lock contention means another process (or
/// another descriptor of this one) holds the stripe.
pub(crate) fn try_sha_lock(
    commits_dir: &Path,
    sha: &[u8; 20],
    role: &str,
) -> io::Result<ecp_core::registry::FileLock> {
    let path = sha_lock_path(commits_dir, sha);
    std::fs::create_dir_all(commits_dir.join(LOCKS_DIR))?;
    let lock = ecp_core::registry::FileLock::try_exclusive(&path)?;
    record_sha_lock_owner(&path, role, sha);
    Ok(lock)
}

impl CommitIndex {
    pub fn scan(commits_dir: &Path) -> io::Result<Self> {
        Ok(Self::from_listing(&CommitDirListing::read(commits_dir)?))
    }

    fn from_listing(listing: &CommitDirListing) -> Self {
        let mut best: FxHashMap<[u8; 20], &CommitDir> = FxHashMap::default();
        for dir in &listing.dirs {
            match best.entry(dir.parsed.sha) {
                Entry::Vacant(slot) => {
                    slot.insert(dir);
                }
                Entry::Occupied(mut slot) => {
                    if rank_cmp(dir, slot.get()) == Ordering::Greater {
                        slot.insert(dir);
                    }
                }
            }
        }
        Self {
            by_sha: best
                .into_iter()
                .map(|(sha, dir)| (sha, dir.name.clone()))
                .collect(),
        }
    }

    /// `scan` + process-level cache keyed on `commits_dir` mtime. A publish
    /// renames into that dir and changes its mtime, so the next call rescans;
    /// two publishes in one timestamp tick are caught by the settle rule and
    /// the monotonic age bound instead. The publishing process drops its own
    /// entry with [`CommitIndex::invalidate`]. Cache miss / unavailable mtime
    /// falls through to plain `scan`. Used by classify in hot-path query setup.
    ///
    /// Returns `Arc<Self>` so callers share the cached map without cloning.
    /// Poisoned lock is recovered via `into_inner()` per Rust stdlib idiom:
    /// poison just means a previous holder panicked, the data itself is
    /// fine — silently swallowing would leave the cache permanently dark.
    pub fn scan_cached(commits_dir: &Path) -> io::Result<Arc<Self>> {
        Self::scan_cached_at(commits_dir, SystemTime::now(), Instant::now())
    }

    /// [`CommitIndex::scan_cached`] with both clocks given, for tests.
    pub fn scan_cached_at(
        commits_dir: &Path,
        wall_now: SystemTime,
        mono_now: Instant,
    ) -> io::Result<Arc<Self>> {
        Self::scan_cached_inner(commits_dir, wall_now, mono_now, || {})
    }

    fn scan_cached_inner(
        commits_dir: &Path,
        wall_now: SystemTime,
        mono_now: Instant,
        before_store: impl FnOnce(),
    ) -> io::Result<Arc<Self>> {
        let Some(mtime) = std::fs::metadata(commits_dir)
            .ok()
            .and_then(|m| m.modified().ok())
        else {
            return Ok(Arc::new(Self::scan(commits_dir)?));
        };
        let cache = scan_cache();
        let epoch = {
            let map = cache.read().unwrap_or_else(|e| e.into_inner());
            let slot = map.get(commits_dir);
            if let Some(hit) = slot.and_then(|s| s.scan.as_ref()) {
                let young = mono_now
                    .checked_duration_since(hit.scanned_at)
                    .is_some_and(|age| age < SCAN_CACHE_MAX_AGE);
                if hit.dir_mtime == mtime && young {
                    return Ok(Arc::clone(&hit.index));
                }
            }
            slot.map_or(0, |s| s.epoch)
        };
        let fresh = Arc::new(Self::scan(commits_dir)?);
        before_store();
        let settled = wall_now
            .duration_since(mtime)
            .is_ok_and(|age| age >= SCAN_CACHE_SETTLE);
        if settled {
            let mut map = cache.write().unwrap_or_else(|e| e.into_inner());
            let slot = map.entry(commits_dir.to_path_buf()).or_default();
            if slot.epoch == epoch {
                slot.scan = Some(CachedScan {
                    dir_mtime: mtime,
                    scanned_at: mono_now,
                    index: Arc::clone(&fresh),
                });
            }
        }
        Ok(fresh)
    }

    /// Drop this process's cached scan of `commits_dir`, and stop any scan
    /// already running from storing its answer. The builder calls it after
    /// its publish, so this process never answers from the pre-publish scan.
    pub fn invalidate(commits_dir: &Path) {
        let mut map = scan_cache().write().unwrap_or_else(|e| e.into_inner());
        let slot = map.entry(commits_dir.to_path_buf()).or_default();
        slot.epoch = slot.epoch.wrapping_add(1);
        slot.scan = None;
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
/// same-SHA tie under [`rank_cmp`], which is the safe default for a
/// half-broken / not-yet-published dir.
fn commit_dir_mtime(path: &Path) -> SystemTime {
    path.join("meta.json")
        .metadata()
        .and_then(|m| m.modified())
        .or_else(|_| path.join("graph.bin").metadata().and_then(|m| m.modified()))
        .or_else(|_| path.metadata().and_then(|m| m.modified()))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Also true for both staging shapes: `.building` and `.building.<pid>.<n>`.
/// Every ecp since generation dirs ships this predicate, so an older binary's
/// readers skip the per-attempt staging shape too.
pub fn is_recovery_dir_name(name: &str) -> bool {
    let Some((_prefix, suffix)) = name.rsplit_once("__") else {
        return false;
    };
    suffix.ends_with(".building")
        || suffix.contains(".building.")
        || suffix.contains(".stale-")
        || suffix.ends_with(".dead")
        || suffix.contains(".dead.")
}

/// The current dir of every SHA under `commits_dir` that has a `graph.bin`,
/// plus the non-commit dirs that have one, each paired with that file's
/// mtime. A superseded generation is never a candidate: after a clock step
/// back its `graph.bin` can be the newest, and gc deletes it next. Returns an
/// empty vec when the dir is absent or unreadable.
fn graph_dirs_with_mtime(commits_dir: &Path) -> Vec<(SystemTime, PathBuf)> {
    let Ok(listing) = CommitDirListing::read(commits_dir) else {
        return Vec::new();
    };
    let current = CommitIndex::from_listing(&listing);
    current
        .by_sha
        .values()
        .chain(listing.unparsed.iter())
        .filter_map(|name| {
            let dir = commits_dir.join(name);
            let mtime = std::fs::metadata(dir.join("graph.bin"))
                .ok()?
                .modified()
                .ok()?;
            Some((mtime, dir))
        })
        .collect()
}

/// Newest `graph.bin` mtime first; equal mtimes by the greater dir name, so
/// the order does not depend on `read_dir` order.
fn newest_first(a: &(SystemTime, PathBuf), b: &(SystemTime, PathBuf)) -> Ordering {
    b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1))
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
        .min_by(newest_first)
        .map(|(_, p)| p)
}

/// Newest published `graph.bin` for a registry repo dir — the graph queries
/// warm-attach and serve when HEAD has no build yet. Shared by group impact,
/// summary coverage reporting, and `doctor coverage`.
pub fn latest_graph_bin(home_ecp: &Path, dir_name: &str) -> Option<PathBuf> {
    find_latest_by_mtime(&home_ecp.join(dir_name).join("commits")).map(|dir| dir.join("graph.bin"))
}

/// Like `find_latest_by_mtime`, but returns ALL candidate commit dirs sorted
/// newest-first. Used by warm-attach to try the next-best sibling when the
/// single newest one fails a downstream gate (compatibility / commit distance)
/// — picking only the newest and giving up there leaves a usable sibling
/// unused on disk.
pub fn find_all_by_mtime_desc(commits_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = graph_dirs_with_mtime(commits_dir);
    dirs.sort_unstable_by(newest_first);
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

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn sha() -> [u8; 20] {
        let mut sha = [0u8; 20];
        hex::decode_to_slice(SHA, &mut sha).unwrap();
        sha
    }

    fn set_mtime(path: &Path, secs: i64) {
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(secs, 0)).unwrap();
    }

    fn dir_with_meta(commits: &Path, name: &str, meta_mtime: i64) -> PathBuf {
        let dir = commits.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), b"{}").unwrap();
        set_mtime(&dir.join("meta.json"), meta_mtime);
        dir
    }

    /// After a clock step back, a superseded generation can carry the newest
    /// `graph.bin`. The cross-SHA lookup must still offer only the current
    /// dir of that SHA, the one gc keeps.
    #[test]
    fn test_find_all_by_mtime_desc_superseded_generation_newest_mtime_not_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        let current = published(commits, &format!("commit__{SHA}.gen.2000.1.0"), 1_000);
        published(commits, &format!("commit__{SHA}.gen.1000.1.0"), 9_000);
        assert_eq!(find_all_by_mtime_desc(commits), vec![current.clone()]);
        assert_eq!(find_latest_by_mtime(commits), Some(current));
    }

    /// Equal `graph.bin` mtimes across SHAs order by name, not by `read_dir`.
    #[test]
    fn test_find_all_by_mtime_desc_equal_mtimes_ordered_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        let a = published(commits, &format!("commit__{}", "a".repeat(40)), 5_000);
        let b = published(commits, &format!("commit__{}", "b".repeat(40)), 5_000);
        assert_eq!(find_all_by_mtime_desc(commits), vec![b.clone(), a]);
        assert_eq!(find_latest_by_mtime(commits), Some(b));
    }

    #[test]
    fn test_commit_dir_listing_read_missing_dir_empty_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let listing = CommitDirListing::read(&tmp.path().join("absent")).unwrap();
        assert!(listing.complete);
        assert!(listing.dirs.is_empty() && listing.staging.is_empty());
    }

    #[test]
    fn test_commit_dir_listing_entry_error_incomplete() {
        let listing = CommitDirListing::from_entries(
            Path::new("commits"),
            [
                Ok((format!("commit__{SHA}"), true)),
                Err(io::Error::other("entry vanished")),
            ],
        );
        assert!(!listing.complete);
        assert_eq!(
            listing.dirs.len(),
            1,
            "readers still answer from what was read"
        );
    }

    /// A regular file named like a commit dir is no candidate: nothing can
    /// be loaded from it, and a rename onto it fails rather than replacing it.
    #[test]
    fn test_commit_dir_listing_regular_file_not_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(format!("commit__{SHA}")), b"x").unwrap();
        let listing = CommitDirListing::read(tmp.path()).unwrap();
        assert!(listing.complete);
        assert!(listing.dirs.is_empty());
        assert!(CommitIndex::scan(tmp.path())
            .unwrap()
            .find(&sha())
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_commit_dir_listing_symlink_not_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let commits = tmp.path().join("commits");
        std::fs::create_dir_all(&commits).unwrap();
        std::os::unix::fs::symlink(&target, commits.join(format!("commit__{SHA}"))).unwrap();
        let listing = CommitDirListing::read(&commits).unwrap();
        assert!(listing.dirs.is_empty());
        assert!(listing.unparsed.is_empty());
    }

    #[test]
    fn test_commit_dir_listing_staging_shapes_parsed_retired_skipped() {
        let base = format!("branch_main__{SHA}");
        let listing = CommitDirListing::from_entries(
            Path::new("commits"),
            [
                Ok((format!("{base}.building"), true)),
                Ok((staging_dir_name(&base, 42, 7), true)),
                Ok((format!("{base}.building.42.7.dead.1.2.3"), true)),
                Ok((format!("{base}.building"), false)),
                Ok((LOCKS_DIR.to_string(), true)),
            ],
        );
        let shapes: Vec<StagingShape> = listing.staging.iter().map(|s| s.shape).collect();
        assert_eq!(
            shapes,
            vec![
                StagingShape::Legacy,
                StagingShape::PerAttempt {
                    pid: 42,
                    attempt: 7
                }
            ]
        );
        assert!(listing.staging.iter().all(|s| s.sha == sha()));
        assert!(listing.dirs.is_empty());
        assert!(listing.unparsed.is_empty(), "the lock dir is no candidate");
        assert_eq!(
            listing.retired,
            vec![format!("{base}.building.42.7.dead.1.2.3")]
        );
    }

    /// Old binaries ship this predicate. It must hide the per-attempt
    /// staging shape from them, or an older reader would rank a half-built
    /// graph.
    #[test]
    fn test_is_recovery_dir_name_new_staging_shape_true() {
        let base = format!("branch_main__{SHA}");
        assert!(is_recovery_dir_name(&staging_dir_name(&base, 1, 0)));
        assert!(is_recovery_dir_name(&format!("{base}.building")));
        assert!(!is_recovery_dir_name(&base));
        assert!(CommitDirName::parse(&staging_dir_name(&base, 1, 0)).is_err());
    }

    /// Two dirs with the same generation and the same mtime: today `scan`
    /// kept whichever `read_dir` returned first and gc kept the last. Both
    /// now keep the greater name.
    #[test]
    fn test_rank_cmp_equal_generation_equal_mtime_greater_name_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        dir_with_meta(commits, &format!("branch_main__{SHA}"), 5_000);
        dir_with_meta(commits, &format!("tag_v1__{SHA}"), 5_000);
        let listing = CommitDirListing::read(commits).unwrap();
        let current = listing.current(&sha()).unwrap();
        assert_eq!(current.name, format!("tag_v1__{SHA}"));
        assert_eq!(
            CommitIndex::scan(commits).unwrap().find(&sha()),
            Some(format!("tag_v1__{SHA}").as_str())
        );
    }

    /// Dirs older binaries wrote: two bare names and a one-field `.gen.<ts>`.
    /// The generation still wins over any mtime, and between the bare names
    /// the newer `meta.json`, as before.
    #[test]
    fn test_commit_index_scan_old_binary_dirs_ranked_as_today() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        dir_with_meta(commits, &format!("branch_main__{SHA}"), 9_000);
        dir_with_meta(commits, &format!("tag_v1__{SHA}"), 8_000);
        let listing = CommitDirListing::read(commits).unwrap();
        assert_eq!(
            listing.current(&sha()).unwrap().name,
            format!("branch_main__{SHA}")
        );
        dir_with_meta(
            commits,
            &format!("branch_main__{SHA}.gen.1730000000000"),
            1_000,
        );
        assert_eq!(
            CommitIndex::scan(commits).unwrap().find(&sha()),
            Some(format!("branch_main__{SHA}.gen.1730000000000").as_str())
        );
    }

    /// No `meta.json` and no `graph.bin`: the dir mtime decides, then the name.
    #[test]
    fn test_commit_index_scan_absent_meta_ranks_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        for name in [format!("branch_main__{SHA}"), format!("commit__{SHA}")] {
            std::fs::create_dir_all(commits.join(&name)).unwrap();
            set_mtime(&commits.join(&name), 7_000);
        }
        assert_eq!(
            CommitIndex::scan(commits).unwrap().find(&sha()),
            Some(format!("commit__{SHA}").as_str())
        );
    }

    /// Moved from `wait_for_completion_*` (deleted with the poll): the
    /// generation tuple dominates mtime, so a base dir whose `meta.json` an
    /// attach just touched never shadows the generation dir.
    #[test]
    fn test_commit_index_scan_generation_dominates_newer_base_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        dir_with_meta(commits, &format!("branch_main__{SHA}"), 9_000_000);
        dir_with_meta(
            commits,
            &format!("branch_main__{SHA}.gen.1730000000000.42.7"),
            1_000,
        );
        assert_eq!(
            CommitIndex::scan(commits).unwrap().find(&sha()),
            Some(format!("branch_main__{SHA}.gen.1730000000000.42.7").as_str())
        );
    }

    /// The accepted mixed-version window: an older binary publishes a lower
    /// generation after a new one. It loses, and gc deletes it.
    #[test]
    fn test_commit_index_scan_lower_generation_published_later_loses() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        dir_with_meta(commits, &format!("commit__{SHA}.gen.2000.1.0"), 1_000);
        dir_with_meta(commits, &format!("commit__{SHA}.gen.1000.9.0"), 9_000);
        assert_eq!(
            CommitIndex::scan(commits).unwrap().find(&sha()),
            Some(format!("commit__{SHA}.gen.2000.1.0").as_str())
        );
    }

    #[test]
    fn test_sha_lock_path_first_byte_stripe_under_locks_dir() {
        let commits = Path::new("commits");
        let path = sha_lock_path(commits, &sha());
        assert_eq!(path, commits.join(LOCKS_DIR).join("01.lock"));
        let mut same_stripe = sha();
        same_stripe[19] ^= 0xff;
        assert_eq!(sha_lock_path(commits, &same_stripe), path);
        let mut other_stripe = sha();
        other_stripe[0] = 0xab;
        assert_eq!(
            sha_lock_path(commits, &other_stripe),
            commits.join(LOCKS_DIR).join("ab.lock")
        );
    }

    /// The lock dir sits among the commit dirs; no scan of `commits/` may
    /// offer it, even with a `graph.bin` in it.
    #[test]
    fn test_commit_dir_listing_locks_dir_excluded_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path();
        let good = published(commits, "commit__aa", 1_000);
        published(commits, LOCKS_DIR, 9_000);
        let listing = CommitDirListing::read(commits).unwrap();
        assert!(listing.dirs.is_empty() && listing.staging.is_empty());
        assert_eq!(listing.unparsed, vec!["commit__aa".to_string()]);
        assert_eq!(find_all_by_mtime_desc(commits), vec![good.clone()]);
        assert_eq!(find_latest_by_mtime(commits), Some(good));
    }

    /// A commits dir whose last change is old enough to be settled.
    fn settled_commits() -> (tempfile::TempDir, PathBuf, SystemTime) {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path().join("commits");
        std::fs::create_dir_all(commits.join(format!("commit__{SHA}"))).unwrap();
        let now = SystemTime::now();
        filetime::set_file_mtime(
            &commits,
            filetime::FileTime::from_system_time(now - Duration::from_secs(60)),
        )
        .unwrap();
        (tmp, commits, now)
    }

    #[test]
    fn test_scan_cached_at_settled_within_age_hits() {
        let (_tmp, commits, wall) = settled_commits();
        let mono = Instant::now();
        let first = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        let second =
            CommitIndex::scan_cached_at(&commits, wall, mono + Duration::from_secs(1)).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    /// The mtime is unchanged, yet the entry is too old to trust: a second
    /// publish in the same tick would have left this mtime too.
    #[test]
    fn test_scan_cached_at_equal_mtime_past_max_age_rescans() {
        let (_tmp, commits, wall) = settled_commits();
        let mono = Instant::now();
        let first = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        let later =
            CommitIndex::scan_cached_at(&commits, wall, mono + Duration::from_secs(3)).unwrap();
        assert!(!Arc::ptr_eq(&first, &later));
    }

    #[test]
    fn test_scan_cached_at_racy_mtime_rescans() {
        let (_tmp, commits, wall) = settled_commits();
        filetime::set_file_mtime(&commits, filetime::FileTime::from_system_time(wall)).unwrap();
        let mono = Instant::now();
        let first = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        let second = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn test_scan_cached_at_future_mtime_rescans() {
        let (_tmp, commits, wall) = settled_commits();
        filetime::set_file_mtime(
            &commits,
            filetime::FileTime::from_system_time(wall + Duration::from_secs(60)),
        )
        .unwrap();
        let mono = Instant::now();
        let first = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        let second = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
    }

    /// A scan that read the dir before a publish, and stores after the
    /// publisher's invalidate, must not put its pre-publish answer back.
    #[test]
    fn test_scan_cached_inner_invalidate_during_scan_not_stored() {
        let (_tmp, commits, wall) = settled_commits();
        let mono = Instant::now();
        let raced = CommitIndex::scan_cached_inner(&commits, wall, mono, || {
            CommitIndex::invalidate(&commits)
        })
        .unwrap();
        let next = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        assert!(!Arc::ptr_eq(&raced, &next), "the raced scan was stored");
        let again = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        assert!(
            Arc::ptr_eq(&next, &again),
            "a scan without a race is stored, so the miss above came from the epoch"
        );
    }

    #[test]
    fn test_invalidate_drops_stored_scan() {
        let (_tmp, commits, wall) = settled_commits();
        let mono = Instant::now();
        let first = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        CommitIndex::invalidate(&commits);
        let second = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
    }
}
