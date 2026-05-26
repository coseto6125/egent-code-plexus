//! Garbage collection: reachability-based LRU eviction + session orphan sweep.
//!
//! Covered by `tests/gc.rs` integration tests.

use crate::git::safe_exec;
use ecp_core::registry::{CommitBuildMeta, CommitDirName};
use ecp_core::session::SessionMeta;
use rustc_hash::{FxHashMap, FxHashSet};
use std::fs;
use std::io;
use std::path::Path;

pub const DEFAULT_QUOTA_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const TARGET_LOAD_FACTOR: f64 = 0.8;
const SESSION_IDLE_HOURS: i64 = 24;
/// A dir/file younger than this may belong to an in-flight build or a writer
/// still mid-`rename`; skip it so gc never races a live producer.
const IN_FLIGHT_SECS: u64 = 10;

/// True when `meta`'s mtime is within [`IN_FLIGHT_SECS`] of `now` — i.e. a
/// producer may still be writing it. Unstatable mtime → treat as settled
/// (returns false) so a stat quirk never wedges gc.
fn is_in_flight(meta: &std::fs::Metadata, now: std::time::SystemTime) -> bool {
    meta.modified()
        .ok()
        .and_then(|m| now.duration_since(m).ok())
        .map(|age| age.as_secs() < IN_FLIGHT_SECS)
        .unwrap_or(false)
}

pub struct EvictStats {
    pub evicted: usize,
    pub freed_bytes: u64,
}

pub struct SweepStats {
    pub marked: usize,
    pub removed: usize,
}

/// SHAs that must be retained: branch/tag/ref objects from the worktree
/// A session dir marked dead: `<sid>.dead` or `<sid>.dead.<unix_ts>` (single
/// trailing epoch segment — the shape `sweep_sessions` writes via
/// `path.with_extension(format!("dead.{ts}"))`).
fn is_session_dead(name: &str) -> bool {
    name.ends_with(".dead")
        || name
            .rsplit_once(".dead.")
            .map(|(_, ts)| ts.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false)
}

/// A retired repo dir: `<repo>__<hash>.dead.<pid>.<n>.<ts>` — three numeric
/// segments, the shape `fs_safe::retire_dir_async` emits (distinct from the
/// single-epoch session form, hence a separate predicate).
pub(crate) fn is_repo_retired(name: &str) -> bool {
    name.ends_with(".dead")
        || name
            .rsplit_once(".dead.")
            .map(|(_, rest)| {
                rest.split('.')
                    .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()))
            })
            .unwrap_or(false)
}

/// plus pinned `base_sha` of active sessions (last_touched within 24h).
pub fn reachability(repo_root: &Path, worktree: &Path) -> io::Result<FxHashSet<String>> {
    let mut set = FxHashSet::default();

    // Branch / tag / ref objects from the live worktree
    if let Ok(out) = safe_exec::git()
        .args(["for-each-ref", "--format=%(objectname)"])
        .current_dir(worktree)
        .output()
    {
        if out.status.success() {
            for line in std::str::from_utf8(&out.stdout).unwrap_or("").lines() {
                let s = line.trim();
                if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
                    set.insert(s.to_string());
                }
            }
        }
    }

    // Active sessions
    let sessions_dir = repo_root.join("sessions");
    if let Ok(it) = fs::read_dir(&sessions_dir) {
        for entry in it.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if is_session_dead(&s) || s.contains(".stale-") {
                continue;
            }
            let sm_path = entry.path().join("session_meta.json");
            let Ok(sm) = SessionMeta::read(&sm_path) else {
                continue;
            };
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&sm.last_touched) {
                let age =
                    chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
                if age.num_hours() > SESSION_IDLE_HOURS {
                    continue;
                }
            }
            set.insert(sm.base_sha);
        }
    }

    Ok(set)
}

pub fn enforce_quota(repo_root: &Path, worktree: &Path, quota: u64) -> io::Result<EvictStats> {
    let reachable = reachability(repo_root, worktree)?;
    let commits_dir = repo_root.join("commits");
    let it = match fs::read_dir(&commits_dir) {
        Ok(d) => d,
        Err(_) => {
            return Ok(EvictStats {
                evicted: 0,
                freed_bytes: 0,
            })
        }
    };

    let mut entries: Vec<(std::path::PathBuf, String, u64, i64)> = vec![];
    for e in it.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.contains(".building") || name.contains(".stale") {
            continue;
        }
        let Ok(parsed_name) = CommitDirName::parse(&name) else {
            continue;
        };
        let meta_path = e.path().join("meta.json");
        let Ok(cm) = CommitBuildMeta::read(&meta_path) else {
            continue;
        };
        let size = dir_size(&e.path()).unwrap_or(0);
        let built_at_epoch = chrono::DateTime::parse_from_rfc3339(&cm.built_at)
            .map(|d| d.timestamp())
            .unwrap_or(0);
        entries.push((e.path(), parsed_name.sha_hex(), size, built_at_epoch));
    }

    let total: u64 = entries.iter().map(|(_, _, s, _)| *s).sum();
    if total <= quota {
        return Ok(EvictStats {
            evicted: 0,
            freed_bytes: 0,
        });
    }
    let target_size = (quota as f64 * TARGET_LOAD_FACTOR) as u64;

    entries.sort_by_key(|(_, _, _, t)| *t); // ASC = oldest first

    let mut freed = 0u64;
    let mut evicted = 0usize;
    let mut current = total;
    for (path, sha, size, _) in entries {
        if current <= target_size {
            break;
        }
        if reachable.contains(&sha) {
            continue;
        }
        fs::remove_dir_all(&path)?;
        current = current.saturating_sub(size);
        freed += size;
        evicted += 1;
    }

    Ok(EvictStats {
        evicted,
        freed_bytes: freed,
    })
}

pub fn sweep_sessions(repo_root: &Path) -> io::Result<SweepStats> {
    let sessions_dir = repo_root.join("sessions");
    let mut marked = 0usize;
    let mut removed = 0usize;
    let Ok(it) = fs::read_dir(&sessions_dir) else {
        return Ok(SweepStats { marked, removed });
    };
    for entry in it.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if is_session_dead(&name) || name.contains(".stale-") {
            fs::remove_dir_all(&path)?;
            removed += 1;
            continue;
        }
        let sm_path = path.join("session_meta.json");
        let Ok(sm) = SessionMeta::read(&sm_path) else {
            continue;
        };

        let mut should_mark = false;

        #[cfg(unix)]
        {
            if let Some(pid) = sm.pid {
                // SAFETY: kill(pid, 0) is a safe signal-zero probe; returns 0 if pid
                // is alive, -1 with errno=ESRCH if not. No state changes.
                unsafe {
                    if libc::kill(pid as i32, 0) != 0 {
                        let e = std::io::Error::last_os_error();
                        if e.raw_os_error() == Some(libc::ESRCH) {
                            should_mark = true;
                        }
                    }
                }
            }
        }

        if !should_mark {
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&sm.last_touched) {
                let age =
                    chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
                if age.num_hours() > SESSION_IDLE_HOURS {
                    should_mark = true;
                }
            }
        }

        if should_mark {
            let ts = chrono::Utc::now().timestamp();
            let dead = path.with_extension(format!("dead.{ts}"));
            fs::rename(&path, &dead)?;
            marked += 1;
        }
    }
    Ok(SweepStats { marked, removed })
}

/// Converge same-SHA generation dirs under `<repo_root>/commits/`: for each SHA,
/// keep only the dir with the greatest `Generation` (a base dir with no `.gen`
/// suffix has `generation == None`, ordering below any `Some(_)`), remove the
/// rest. Same SHA → identical graph (ingest is idempotent), so older generations
/// are pure waste. Skips dirs whose mtime is < 10s old or that have a sibling
/// `.building` marker (another session may be mid-ingest). Reuses
/// `CommitDirName::parse` rather than hand-rolling the name grammar.
pub fn sweep_stale_generations(repo_root: &Path) -> io::Result<SweepStats> {
    let commits = repo_root.join("commits");
    let mut removed = 0usize;
    let Ok(it) = fs::read_dir(&commits) else {
        return Ok(SweepStats { marked: 0, removed });
    };

    // First pass: collect SHAs with an active build.
    // Real markers are `<base_dirname>.building` (append-style, per orchestrator.rs:71/682/756),
    // keyed on the commit SHA — never on a specific generation dir.
    let mut building_shas: FxHashSet<[u8; 20]> = FxHashSet::default();
    let mut by_sha: FxHashMap<[u8; 20], Vec<(CommitDirName, std::path::PathBuf)>> =
        FxHashMap::default();
    let now = std::time::SystemTime::now();
    for entry in it.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if name.contains(".building") {
            // Strip the `.building` suffix and parse the remainder to get the SHA.
            if let Some(base) = name.strip_suffix(".building") {
                if let Ok(parsed) = CommitDirName::parse(base) {
                    building_shas.insert(parsed.sha);
                }
            }
            continue;
        }
        let Ok(parsed) = CommitDirName::parse(&name) else {
            continue;
        };
        if is_in_flight(&meta, now) {
            continue;
        }
        by_sha
            .entry(parsed.sha)
            .or_default()
            .push((parsed, entry.path()));
    }

    for (sha, mut group) in by_sha {
        if group.len() < 2 {
            continue;
        }
        if building_shas.contains(&sha) {
            continue;
        }
        group.sort_by_key(|(parsed, _)| parsed.generation);
        let keep_idx = group.len() - 1;
        for (i, (_, path)) in group.iter().enumerate() {
            if i == keep_idx {
                continue;
            }
            match fs::remove_dir_all(path) {
                Ok(()) => removed += 1,
                Err(e) => eprintln!(
                    "gc: failed to remove stale generation {}: {e}",
                    path.display()
                ),
            }
        }
    }

    Ok(SweepStats { marked: 0, removed })
}

/// Remove top-level retired repo dirs (`<repo>__<hash>.dead.<pid>.<n>.<ts>`)
/// left behind when `fs_safe::retire_dir_async`'s background delete thread died
/// with the process before finishing. Already marked dead → removal is
/// unconditional. The `.dead.` infix with a trailing all-digit timestamp segment
/// is the marker (mirrors `sweep_sessions`' dead-detection).
pub fn sweep_retired_repos(home_ecp: &Path) -> io::Result<SweepStats> {
    let mut removed = 0usize;
    let Ok(it) = fs::read_dir(home_ecp) else {
        return Ok(SweepStats { marked: 0, removed });
    };
    for entry in it.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_repo_retired(&name) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) => eprintln!("gc: failed to remove retired repo {}: {e}", path.display()),
        }
    }
    Ok(SweepStats { marked: 0, removed })
}

/// Remove orphaned atomic-write temp siblings (`<name>.<pid>.<n>.tmp`) left in
/// `home_ecp` when a writer's `tmp → fsync → rename` (registry/io.rs) was
/// interrupted before the rename. The io.rs doc comment promises these "can be
/// swept by cleanup tools" — this is that tool. Settled files only (see
/// [`is_in_flight`]), so a live writer mid-`rename` is never touched.
pub fn sweep_orphan_tmp(home_ecp: &Path) -> io::Result<SweepStats> {
    let mut removed = 0usize;
    let Ok(it) = fs::read_dir(home_ecp) else {
        return Ok(SweepStats { marked: 0, removed });
    };
    let now = std::time::SystemTime::now();
    for entry in it.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".tmp") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || is_in_flight(&meta, now) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) => eprintln!(
                "gc: failed to remove orphan tmp {}: {e}",
                entry.path().display()
            ),
        }
    }
    Ok(SweepStats { marked: 0, removed })
}

fn dir_size(dir: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for e in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        if e.file_type().is_file() {
            total += e.metadata()?.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// Backdate a file's mtime past the 10s in-flight guard so the sweep
    /// treats it as a settled orphan rather than a live writer's temp.
    fn backdate(path: &Path, secs: u64) {
        let when = SystemTime::now() - Duration::from_secs(secs);
        let _ = filetime::set_file_mtime(path, filetime::FileTime::from_system_time(when));
    }

    #[test]
    fn sweep_orphan_tmp_removes_settled_tmp_keeps_fresh_and_non_tmp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();

        let stale = home.join("registry.json.10143.0.tmp");
        fs::write(&stale, b"{}").expect("write stale");
        backdate(&stale, 30);

        let fresh = home.join("registry.json.20000.0.tmp");
        fs::write(&fresh, b"{}").expect("write fresh");

        let keep = home.join("registry.json");
        fs::write(&keep, b"{}").expect("write registry");

        let stats = sweep_orphan_tmp(home).expect("sweep");

        assert_eq!(stats.removed, 1, "only the settled .tmp is removed");
        assert!(!stale.exists(), "settled orphan tmp deleted");
        assert!(fresh.exists(), "fresh tmp (live writer) preserved");
        assert!(keep.exists(), "non-tmp registry untouched");
    }

    #[test]
    fn sweep_orphan_tmp_ignores_tmp_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let tmp_dir = home.join("something.5.0.tmp");
        fs::create_dir(&tmp_dir).expect("mkdir");
        backdate(&tmp_dir, 30);

        let stats = sweep_orphan_tmp(home).expect("sweep");
        assert_eq!(stats.removed, 0, "dirs named *.tmp are not files; skipped");
        assert!(tmp_dir.exists());
    }
}
