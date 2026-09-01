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
    /// `(gitdir, common_dir)` from the file readers, `None` when they
    /// declined and the spawn answered instead.
    git_dirs: HashMap<PathBuf, Option<(PathBuf, PathBuf)>>,
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
    if let Some(sha) = head_sha_from_files(cwd) {
        return Some(sha);
    }
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

/// Worktree root for a git `common_dir` (`<worktree>/.git`) — its parent.
/// Falls back to `common_dir` itself when it has no parent (defensive: a
/// bare-repo or root path). Used wherever a registry entry's `.git` common
/// dir must be turned into the source tree `ensure_fresh` walks.
///
/// `dunce::simplified` strips any Windows verbatim `\\?\` prefix the registry
/// may carry — older builds wrote `common_dir` via `std::fs::canonicalize`,
/// which emits UNC/verbatim paths on Windows. Feeding such a path straight to
/// `ignore::WalkBuilder` / `git archive` makes them treat a file component as
/// a directory and fail with `ERROR_DIRECTORY` (os error 267). On non-Windows
/// and on already-plain paths this is a no-op, returning a sub-slice of the
/// input (no allocation, borrow preserved).
pub fn worktree_root_from_common_dir(common_dir: &Path) -> &Path {
    dunce::simplified(common_dir.parent().unwrap_or(common_dir))
}

fn read_common_dir(cwd: &Path) -> io::Result<PathBuf> {
    if let Some(dir) = common_dir_from_files(cwd) {
        return Ok(dir);
    }
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

// ── File-based readers ───────────────────────────────────────────────────────
//
// `git rev-parse HEAD` and `git rev-parse --git-common-dir` cost ~1 ms each as
// a child process, and every graph-backed command plus every hook paid both.
// The answers live in a handful of small files, so read those and keep the
// spawn as the fallback for every layout not modelled here (bare repos,
// symbolic-ref chains, the reftable backend, `GIT_DIR`-style overrides, a
// repository git would refuse for ownership).

/// Environment overrides redirect git away from the on-disk layout under
/// `cwd`; the file readers cannot see them, so they hand back to the spawn.
fn git_env_overrides_present() -> bool {
    [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_CEILING_DIRECTORIES",
    ]
    .iter()
    .any(|k| std::env::var_os(k).is_some())
}

/// The worktree's own gitdir and its common dir, cached per canonical cwd
/// for the process. `head_sha` and `common_dir` both need the pair, and a
/// fresh walk for each would canonicalize and stat the same entries twice.
fn git_dirs(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    if git_env_overrides_present() {
        return None;
    }
    let key = canon_key(cwd);
    if let Ok(guard) = cache().lock() {
        if let Some(cached) = guard.git_dirs.get(&key) {
            return cached.clone();
        }
    }
    let computed = discover_gitdir(&key).and_then(|gitdir| {
        let common = common_dir_of_gitdir(&gitdir)?;
        Some((gitdir, common))
    });
    if let Ok(mut guard) = cache().lock() {
        guard.git_dirs.insert(key, computed.clone());
    }
    computed
}

/// The worktree's own gitdir: `<root>/.git` when that is a directory, or the
/// `gitdir:` target when it is a file (linked worktrees, submodules). `start`
/// is already canonical.
fn discover_gitdir(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        let gitdir = if meta.is_dir() {
            dot_git
        } else {
            let content = fs::read_to_string(&dot_git).ok()?;
            let target = Path::new(content.strip_prefix("gitdir:")?.trim());
            let abs = if target.is_absolute() {
                target.to_path_buf()
            } else {
                dir.join(target)
            };
            fs::canonicalize(abs).ok()?
        };
        // A `.git` entry without a `HEAD` is not a repository to git either
        // (a stub or a vendored fragment); answering for it would turn a
        // plain source tree into a half-git one.
        return fs::metadata(gitdir.join("HEAD"))
            .ok()
            .filter(|m| m.is_file())
            .map(|_| gitdir);
    }
    None
}

/// The shared dir behind a gitdir: `commondir` redirects a linked worktree,
/// otherwise the gitdir is its own common dir. `None` when the result is not
/// something git would accept, so the spawn gets to refuse it the same way.
fn common_dir_of_gitdir(gitdir: &Path) -> Option<PathBuf> {
    let common = match fs::read_to_string(gitdir.join("commondir")) {
        Ok(rel) => {
            let target = Path::new(rel.trim());
            let abs = if target.is_absolute() {
                target.to_path_buf()
            } else {
                gitdir.join(target)
            };
            fs::canonicalize(abs).ok()?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => gitdir.to_path_buf(),
        Err(_) => return None,
    };
    let is_repository =
        common.join("objects").is_dir() && common.join("refs").is_dir() && owned_by_caller(gitdir);
    is_repository.then_some(common)
}

/// git refuses a repository owned by another user (`safe.directory`), and
/// the readers must not answer where git would not; `safe.directory`
/// exceptions fall through to the spawn, which honours them.
#[cfg(unix)]
fn owned_by_caller(gitdir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    fs::metadata(gitdir).is_ok_and(|m| m.uid() == euid)
}

#[cfg(not(unix))]
fn owned_by_caller(_gitdir: &Path) -> bool {
    true
}

/// `git rev-parse --git-common-dir`, read from `.git` / `commondir` files.
/// `None` means "not modelled here", not "not a repo": the caller spawns git.
pub fn common_dir_from_files(cwd: &Path) -> Option<PathBuf> {
    git_dirs(cwd).map(|(_, common)| common)
}

/// `git rev-parse HEAD`, read from `HEAD`, the loose ref and `packed-refs`.
/// `None` on an unborn branch, a symbolic-ref chain, or any layout not
/// modelled here: the caller spawns git and keeps its own fallbacks.
pub fn head_sha_from_files(cwd: &Path) -> Option<String> {
    let (gitdir, common) = git_dirs(cwd)?;
    let head = fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(refname) = head.strip_prefix("ref:") else {
        return hex_object_id(head);
    };
    let refname = refname.trim();
    // A loose ref overrides a packed one, exactly as git resolves it. Branch
    // refs are shared and live under the common dir; per-worktree refs
    // (`refs/bisect`, `refs/worktree`) live under the worktree's own gitdir.
    let loose = fs::read_to_string(common.join(refname))
        .or_else(|_| fs::read_to_string(gitdir.join(refname)))
        .ok();
    if let Some(content) = loose {
        return hex_object_id(content.trim());
    }
    packed_ref_object_id(&common.join("packed-refs"), refname)
}

/// A full SHA-1 (40) or SHA-256 (64) hex object id, lowercased the way
/// `git rev-parse` prints it.
fn hex_object_id(s: &str) -> Option<String> {
    let full_length = s.len() == 40 || s.len() == 64;
    (full_length && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

fn packed_ref_object_id(packed_refs: &Path, refname: &str) -> Option<String> {
    let text = fs::read_to_string(packed_refs).ok()?;
    text.lines()
        // `#` is the header; `^` lines carry the peeled target of the tag above.
        .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
        .find_map(|line| {
            let (oid, name) = line.split_once(' ')?;
            (name.trim() == refname)
                .then(|| hex_object_id(oid))
                .flatten()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_root_drops_dot_git_component() {
        let common = Path::new("/home/me/proj/.git");
        assert_eq!(
            worktree_root_from_common_dir(common),
            Path::new("/home/me/proj")
        );
    }

    #[test]
    fn worktree_root_falls_back_when_no_parent() {
        // A root path has no parent — return it unchanged rather than panic.
        let root = Path::new("/");
        assert_eq!(worktree_root_from_common_dir(root), root);
    }

    // ── Windows-only: verbatim `\\?\` prefix handling ───────────────────────
    // Registries written by older builds (`std::fs::canonicalize`) store a
    // verbatim `common_dir`; the worktree root handed to `ensure_fresh` /
    // `build_l2` must be plain, else `ignore::WalkBuilder` / `git archive`
    // fail with ERROR_DIRECTORY (os error 267). `dunce::simplified` is a no-op
    // on non-Windows, so these assertions are meaningful only on Windows.

    #[cfg(windows)]
    #[test]
    fn worktree_root_strips_verbatim_disk_prefix() {
        let common = Path::new(r"\\?\C:\Revice_Code\backstage_api_test_new\.git");
        assert_eq!(
            worktree_root_from_common_dir(common),
            Path::new(r"C:\Revice_Code\backstage_api_test_new")
        );
    }

    #[cfg(windows)]
    #[test]
    fn worktree_root_leaves_plain_path_untouched() {
        // Already-plain paths (the post-fix writer output) must pass through
        // unchanged — the simplification is idempotent.
        let common = Path::new(r"C:\Revice_Code\backstage_api_test_new\.git");
        assert_eq!(
            worktree_root_from_common_dir(common),
            Path::new(r"C:\Revice_Code\backstage_api_test_new")
        );
    }
}
