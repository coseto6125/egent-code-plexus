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

pub struct EvictStats {
    pub evicted: usize,
    pub freed_bytes: u64,
}

pub struct SweepStats {
    pub marked: usize,
    pub removed: usize,
}

/// SHAs that must be retained: branch/tag/ref objects from the worktree
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
            let is_dead = s.ends_with(".dead")
                || s.rsplit_once(".dead.")
                    .map(|(_, ts)| ts.chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or(false);
            if is_dead || s.contains(".stale-") {
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
        let is_dead = name.ends_with(".dead")
            || name
                .rsplit_once(".dead.")
                .map(|(_, ts)| ts.chars().all(|c| c.is_ascii_digit()))
                .unwrap_or(false);
        if is_dead || name.contains(".stale-") {
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
        let path = entry.path();
        if !path.is_dir() {
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
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if now
                    .duration_since(modified)
                    .map(|d| d.as_secs() < 10)
                    .unwrap_or(false)
                {
                    continue;
                }
            }
        }
        by_sha.entry(parsed.sha).or_default().push((parsed, path));
    }

    for (sha, mut group) in by_sha {
        if group.len() < 2 {
            continue;
        }
        // A build in progress for this SHA (real markers are `<dirname>.building`,
        // append-style, keyed on the SHA per orchestrator.rs). Never GC a generation
        // while its SHA is being rebuilt.
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
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dead = name.ends_with(".dead")
            || name
                .rsplit_once(".dead.")
                .map(|(_, rest)| {
                    rest.split('.')
                        .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()))
                })
                .unwrap_or(false);
        if !is_dead {
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) => eprintln!("gc: failed to remove retired repo {}: {e}", path.display()),
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
