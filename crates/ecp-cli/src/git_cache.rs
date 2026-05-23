//! Process-wide memoization for git subprocess calls.
//!
//! Warm-query startup invokes `git rev-parse HEAD` 3-4 times across
//! `graph_path::resolve` (twice in `main.rs`), `auto_ensure::ensure_index`'s
//! fingerprint shortcut, and `apply_l1_overlay_updates`. Each subprocess
//! fork+exec costs ~1-3ms; combined with `git rev-parse --git-common-dir`
//! resolution in `repo_identity` that's 5-12ms of pure startup overhead on
//! every command — visible in the 10ms warm-query budget.
//!
//! Cache is keyed by canonical cwd. HEAD entries piggy-back on the current
//! HEAD target's mtime so mid-process commits/checkouts (`ecp diff` does this
//! via `GitGuard`) transparently invalidate without explicit `clear()` calls.
//! Common-dir entries cache for the process lifetime — git's common-dir does
//! not move under us.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use std::{fs, io};

use crate::git::safe_exec;

#[derive(Default)]
struct Cache {
    /// `(value, HEAD-target-mtime)` — mtime stamped from the loose ref pointed
    /// at by `<common_dir>/HEAD`, or HEAD itself for detached checkouts.
    /// On hit, restat and invalidate on mismatch.
    head_sha: HashMap<PathBuf, (Option<String>, Option<SystemTime>)>,
    common_dir: HashMap<PathBuf, io::Result<PathBuf>>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: std::sync::OnceLock<Mutex<Cache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

/// Canonicalize the cwd for cache keying. Falls back to the input path on
/// canonicalize failure so non-git dirs still hit the same key consistently.
fn canon_key(cwd: &Path) -> PathBuf {
    std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf())
}

/// Cached `git rev-parse HEAD` → 40-char hex. None when not a git repo or git
/// fails. Cache key is canonical cwd; HEAD mutations (`git commit`, `git
/// checkout`, etc.) invalidate transparently via HEAD target mtime.
pub fn head_sha(cwd: &Path) -> Option<String> {
    let key = canon_key(cwd);
    let head_mtime = head_file_mtime(cwd);
    {
        let guard = cache().lock().ok()?;
        if let Some((v, mt)) = guard.head_sha.get(&key) {
            if *mt == head_mtime {
                return v.clone();
            }
        }
    }
    let computed = read_head_sha(cwd);
    if let Ok(mut guard) = cache().lock() {
        guard.head_sha.insert(key, (computed.clone(), head_mtime));
    }
    computed
}

/// mtime of HEAD's current target — sentinel for cache invalidation. For a
/// normal branch this is `.git/refs/heads/<branch>`, because `.git/HEAD`
/// usually stays unchanged across commits. Detached HEAD uses `.git/HEAD`.
/// Returns `None` when no git sentinel is statable; the caller treats
/// `None == None` as a valid cache hit for non-git synthetic SHAs.
fn head_file_mtime(cwd: &Path) -> Option<SystemTime> {
    let common = common_dir(cwd).ok()?;
    let head = common.join("HEAD");
    let head_content = fs::read_to_string(&head).ok()?;
    let path = head_content
        .strip_prefix("ref:")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or(head, |r| common.join(r));
    fs::metadata(&path)
        .or_else(|_| fs::metadata(common.join("packed-refs")))
        .ok()
        .and_then(|m| m.modified().ok())
}

/// Cached `git rev-parse HEAD` parsed into 20 raw bytes. `None` on any
/// failure or non-40-hex output (same contract as the prior
/// `graph_path::head_sha_bytes`).
pub fn head_sha_bytes(cwd: &Path) -> Option<[u8; 20]> {
    let s = head_sha(cwd)?;
    if s.len() != 40 {
        return None;
    }
    let mut sha = [0u8; 20];
    hex::decode_to_slice(&s, &mut sha).ok()?;
    Some(sha)
}

fn read_head_sha(cwd: &Path) -> Option<String> {
    let out = safe_exec::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if out.status.success() {
        let s = std::str::from_utf8(&out.stdout).ok()?.trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    // Non-git fallback: synthesize a stable 40-hex digest from canonical path.
    // Mirrors `orchestrator::head_sha_hex` so cache identity stays consistent
    // between the writer (build_l2) and the reader (graph_path::resolve_v2).
    let canonical = std::fs::canonicalize(cwd).ok()?;
    let h = xxhash_rust::xxh3::xxh3_128(canonical.to_string_lossy().as_bytes());
    Some(format!("{h:040x}"))
}

/// Cached `git rev-parse --git-common-dir`. Returns the resolved absolute path
/// (relative output is joined onto `cwd` to preserve the prior
/// `repo_identity::git_common_dir` contract).
pub fn common_dir(cwd: &Path) -> io::Result<PathBuf> {
    let key = canon_key(cwd);
    {
        let guard = cache()
            .lock()
            .map_err(|_| io::Error::other("git_cache mutex poisoned"))?;
        if let Some(cached) = guard.common_dir.get(&key) {
            return cached
                .as_ref()
                .cloned()
                .map_err(|e| io::Error::new(e.kind(), e.to_string()));
        }
    }
    let computed = read_common_dir(cwd);
    let to_return = computed
        .as_ref()
        .cloned()
        .map_err(|e| io::Error::new(e.kind(), e.to_string()));
    if let Ok(mut guard) = cache().lock() {
        guard.common_dir.insert(key, computed);
    }
    to_return
}

fn read_common_dir(cwd: &Path) -> io::Result<PathBuf> {
    let out = safe_exec::git()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("not a git repository"));
    }
    let path_str = std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim();
    let p = PathBuf::from(path_str);
    if p.is_absolute() {
        Ok(p)
    } else {
        Ok(cwd.join(p))
    }
}
