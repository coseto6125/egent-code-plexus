//! Garbage collection: reachability-based LRU eviction + session orphan sweep.
//!
//! Covered by `tests/gc.rs` integration tests; bin compilation sees zero
//! callers because the `admin gc` subcommand isn't wired yet — lift the
//! module allow when it lands.
#![allow(dead_code)]

use crate::git::safe_exec;
use cgn_core::registry::{CommitBuildMeta, CommitDirName};
use cgn_core::session::SessionMeta;
use rustc_hash::FxHashSet;
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
                || s.rsplit_once(".dead.").map(|(_, ts)| ts.chars().all(|c| c.is_ascii_digit())).unwrap_or(false);
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
            || name.rsplit_once(".dead.").map(|(_, ts)| ts.chars().all(|c| c.is_ascii_digit())).unwrap_or(false);
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
