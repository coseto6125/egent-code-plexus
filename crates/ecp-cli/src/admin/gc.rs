//! Garbage collection: reachability-based LRU eviction + session orphan sweep.
//!
//! Covered by `tests/gc.rs` integration tests.

use crate::git::safe_exec;
use ecp_core::registry::{CommitBuildMeta, CommitDirName};
use ecp_core::session::{DirtyFiles, SessionMeta};
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
/// A directory marked dead: `<name>.dead`, or `.dead.` followed by any number
/// of numeric segments.
///
/// One predicate for both shapes on purpose. `sweep_sessions` writes
/// `.dead.<unix_ts>` and `fs_safe::retire_dir_async` writes
/// `.dead.<pid>.<n>.<ts>`; a session retired through the second form went
/// unrecognised while these were two predicates, so it was never swept. The
/// single-segment shape satisfies "every segment is numeric" already, which is
/// why the stricter one could be dropped rather than kept alongside.
pub fn is_retired_dir(name: &str) -> bool {
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

    insert_active_session_bases(repo_root, &mut set);
    Ok(set)
}

fn insert_active_session_bases(repo_root: &Path, set: &mut FxHashSet<String>) {
    let Ok(it) = fs::read_dir(repo_root.join("sessions")) else {
        return;
    };
    for entry in it.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if is_retired_dir(&s) || s.contains(".stale-") {
            continue;
        }
        let sm_path = entry.path().join("session_meta.json");
        let Ok(sm) = SessionMeta::read(&sm_path) else {
            continue;
        };
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&sm.last_touched) {
            let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
            if age.num_hours() > SESSION_IDLE_HOURS {
                continue;
            }
        }
        set.insert(sm.base_sha);
    }
}

/// A commit graph no ref, no worktree HEAD and no live session points at,
/// built longer ago than this, is reclaimed. A merged branch or a deleted
/// worktree otherwise leaves every graph it built behind: `enforce_quota`
/// only acts above 5 GiB per repo. The grace covers short-lived use such as
/// `impact --baseline <old ref>`; a swept graph is rebuilt on demand.
const UNREACHABLE_COMMIT_IDLE_SECS: i64 = 7 * 86_400;

/// Every SHA a ref or a worktree HEAD of the repo at `common_dir` names, or
/// `None` when git cannot answer. Only a complete answer may justify a
/// delete: an empty set from a failed git call would mark every graph dead.
fn live_commit_shas(repo_root: &Path, common_dir: &Path) -> Option<FxHashSet<String>> {
    let git = |args: &[&str]| {
        let out = safe_exec::git()
            .arg("--git-dir")
            .arg(common_dir)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let is_sha = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let mut set: FxHashSet<String> = git(&["for-each-ref", "--format=%(objectname)"])?
        .lines()
        .map(str::trim)
        .filter(|s| is_sha(s))
        .map(str::to_owned)
        .collect();
    // A detached worktree HEAD is not a ref.
    set.extend(
        git(&["worktree", "list", "--porcelain"])?
            .lines()
            .filter_map(|line| line.strip_prefix("HEAD "))
            .map(str::trim)
            .filter(|s| is_sha(s))
            .map(str::to_owned),
    );
    insert_active_session_bases(repo_root, &mut set);
    Some(set)
}

/// Retires commit graphs whose SHA is not live (see [`live_commit_shas`])
/// and that were built more than [`UNREACHABLE_COMMIT_IDLE_SECS`] ago.
/// Removes nothing when the repo's git dir is unknown or git fails.
pub fn sweep_unreachable_commits(repo_root: &Path) -> io::Result<SweepStats> {
    let mut stats = SweepStats {
        marked: 0,
        removed: 0,
    };
    let Ok(it) = fs::read_dir(repo_root.join("commits")) else {
        return Ok(stats);
    };
    let now = std::time::SystemTime::now();
    // Pass 1, stat only: a dir modified within the grace period cannot have
    // been built before it, so a repo with no older dir pays no git call.
    let mut candidates = Vec::new();
    for entry in it.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        // Leftover of an earlier retire whose delete did not finish.
        if is_retired_dir(&name) {
            stats.removed += usize::from(remove_retired(&dir));
            continue;
        }
        let idle_by_mtime = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age.as_secs() >= UNREACHABLE_COMMIT_IDLE_SECS as u64);
        if name.starts_with('.')
            || name.contains(".building")
            || name.contains(".stale")
            || !idle_by_mtime
        {
            continue;
        }
        let Ok(parsed) = CommitDirName::parse(&name) else {
            continue;
        };
        candidates.push((dir, parsed.sha_hex()));
    }
    if candidates.is_empty() {
        return Ok(stats);
    }
    let Ok(repo_meta) = ecp_core::registry::RepoMeta::read(&repo_root.join("meta.json")) else {
        return Ok(stats);
    };
    let common_dir = Path::new(&repo_meta.common_dir);
    if !common_dir.is_dir() {
        return Ok(stats);
    }
    let Some(live) = live_commit_shas(repo_root, common_dir) else {
        return Ok(stats);
    };
    for (dir, sha) in candidates {
        if live.contains(&sha) {
            continue;
        }
        let Ok(cm) = CommitBuildMeta::read(&dir.join("meta.json")) else {
            continue;
        };
        let old_enough = chrono::DateTime::parse_from_rfc3339(&cm.built_at).is_ok_and(|built| {
            chrono::Utc::now()
                .signed_duration_since(built.with_timezone(&chrono::Utc))
                .num_seconds()
                >= UNREACHABLE_COMMIT_IDLE_SECS
        });
        if !old_enough {
            continue;
        }
        stats.marked += 1;
        stats.removed += usize::from(retire_and_remove(&dir));
    }
    Ok(stats)
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
        if is_retired_dir(&name) || name.contains(".stale-") {
            fs::remove_dir_all(&path)?;
            removed += 1;
            continue;
        }
        let sm_path = path.join("session_meta.json");
        let Ok(sm) = SessionMeta::read(&sm_path) else {
            continue;
        };

        // Idle age is the whole test. Probing `sm.pid` used to short-circuit
        // this, but that pid is the one-shot `ecp` process that wrote the
        // meta: it is always dead, so every session was marked on the first
        // sweep after enrolment (FU-2026-06-10-cc120f78889c). A live watcher
        // or a live agent keeps `last_touched` moving instead.
        let should_mark = chrono::DateTime::parse_from_rfc3339(&sm.last_touched).is_ok_and(|t| {
            chrono::Utc::now()
                .signed_duration_since(t.with_timezone(&chrono::Utc))
                .num_hours()
                > SESSION_IDLE_HOURS
        });

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
/// Retire commit dirs whose recorded worktree still exists but is not a
/// worktree root (`orchestrator::built_below_worktree_root`): they were
/// published from a subdirectory and hold that subtree's graph only. A
/// recorded worktree that is gone is left alone; the query path rebuilds such
/// a slot on first use.
pub fn sweep_subtree_builds(repo_root: &Path) -> io::Result<SweepStats> {
    let mut stats = SweepStats {
        marked: 0,
        removed: 0,
    };
    let Ok(it) = fs::read_dir(repo_root.join("commits")) else {
        return Ok(stats);
    };
    for entry in it.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !dir.is_dir()
            || name.starts_with('.')
            || name.contains(".building")
            || is_retired_dir(&name)
        {
            continue;
        }
        let Ok(meta) = ecp_core::registry::CommitBuildMeta::read(&dir.join("meta.json")) else {
            continue;
        };
        let built = Path::new(&meta.built_from_worktree);
        // `Path::new("")` as the root in use: no live root is exempt here.
        if built.is_dir()
            && crate::build::orchestrator::built_below_worktree_root(
                built,
                &meta.sha,
                Path::new(""),
            )
        {
            stats.marked += 1;
            stats.removed += usize::from(retire_and_remove(&dir));
        }
    }
    Ok(stats)
}

/// Every release and every `BUILDER_FINGERPRINT` bump opens a new
/// `parse_cache/<fp>/` dir and never reads the old one again. The grace
/// period keeps a second installed ecp version's live cache from being
/// evicted on each gc, which would make the two binaries re-parse in turn.
const PARSE_CACHE_IDLE_SECS: u64 = 7 * 86_400;

/// Removes `parse_cache/<fp>/` dirs of other fingerprints that no writer
/// touched for [`PARSE_CACHE_IDLE_SECS`], plus `.dead.*` leftovers of an
/// earlier retire whose background delete did not finish.
pub fn sweep_stale_parse_caches(repo_root: &Path) -> io::Result<SweepStats> {
    let mut stats = SweepStats {
        marked: 0,
        removed: 0,
    };
    let Ok(it) = fs::read_dir(repo_root.join("parse_cache")) else {
        return Ok(stats);
    };
    let current = crate::parse_cache::ParseCache::current_dir_name();
    let now = std::time::SystemTime::now();
    for entry in it.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let dir = entry.path();
        if is_retired_dir(&name) {
            stats.removed += usize::from(remove_retired(&dir));
            continue;
        }
        let idle = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age.as_secs() >= PARSE_CACHE_IDLE_SECS);
        if name == current || !idle {
            continue;
        }
        stats.marked += 1;
        stats.removed += usize::from(retire_and_remove(&dir));
    }
    Ok(stats)
}

/// Removes commit dirs whose `graph.bin` is in an older
/// `GRAPH_FORMAT_VERSION` layout, which this binary never reads again: it
/// rebuilds such a slot. The same grace as [`sweep_stale_parse_caches`]
/// applies, because a second installed ecp version may still read them.
pub fn sweep_old_format_commits(repo_root: &Path) -> io::Result<SweepStats> {
    let mut stats = SweepStats {
        marked: 0,
        removed: 0,
    };
    let Ok(it) = fs::read_dir(repo_root.join("commits")) else {
        return Ok(stats);
    };
    let now = std::time::SystemTime::now();
    for entry in it.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !meta.is_dir()
            || name.starts_with('.')
            || name.contains(".building")
            || name.contains(".stale")
            || is_retired_dir(&name)
        {
            continue;
        }
        let idle = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age.as_secs() >= PARSE_CACHE_IDLE_SECS);
        let graph = entry.path().join("graph.bin");
        if !idle || !graph.is_file() || !older_graph_format(&graph) {
            continue;
        }
        stats.marked += 1;
        stats.removed += usize::from(retire_and_remove(&entry.path()));
    }
    Ok(stats)
}

/// Whether `graph` is in an older layout than this binary reads: its
/// version sidecar says so, or, with no sidecar (a binary before the
/// sidecar, or a lost detached write), its header fails validation.
fn older_graph_format(graph: &Path) -> bool {
    match crate::auto_ensure::read_compatible_version_sidecar(graph) {
        Some(version) => version < ecp_core::graph::GRAPH_FORMAT_VERSION,
        None => !crate::engine::header_compatible(graph),
    }
}

/// A fragment bin younger than this may belong to a writer that renamed it
/// in and has not yet committed the manifest naming it.
const ORPHAN_FRAGMENT_IDLE_SECS: u64 = 3600;

/// Removes the fragment bins of each session that its `dirty_files.json`
/// no longer names: bins are content-addressed, so every save of a dirty
/// file writes a new one, and a manifest another builder wrote is replaced
/// whole. Leftover `.tmp` writes go too. A session whose manifest cannot be
/// read keeps its bins.
pub fn sweep_stale_fragments(repo_root: &Path) -> io::Result<SweepStats> {
    let mut stats = SweepStats {
        marked: 0,
        removed: 0,
    };
    let Ok(sessions) = fs::read_dir(repo_root.join("sessions")) else {
        return Ok(stats);
    };
    let now = std::time::SystemTime::now();
    for session in sessions.flatten() {
        let name = session.file_name().to_string_lossy().to_string();
        if is_retired_dir(&name) || name.contains(".stale-") {
            continue;
        }
        let dir = session.path();
        let Ok(bins) = fs::read_dir(dir.join("graph_overlay")) else {
            continue;
        };
        let referenced: FxHashSet<String> = match DirtyFiles::read(&dir.join("dirty_files.json")) {
            Ok(manifest) => manifest
                .entries
                .into_values()
                .map(|entry| entry.fragment_id)
                .collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => FxHashSet::default(),
            Err(_) => continue,
        };
        for bin in bins.flatten() {
            let file_name = bin.file_name().to_string_lossy().to_string();
            let orphan = match file_name.strip_suffix(".bin") {
                Some(fragment_id) => !referenced.contains(fragment_id),
                None => file_name.ends_with(".tmp"),
            };
            let Ok(meta) = bin.metadata() else {
                continue;
            };
            let settled = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age.as_secs() >= ORPHAN_FRAGMENT_IDLE_SECS);
            if !orphan || !meta.is_file() || !settled {
                continue;
            }
            stats.marked += 1;
            match fs::remove_file(bin.path()) {
                Ok(()) => stats.removed += 1,
                Err(e) => eprintln!("gc: remove {}: {e}", bin.path().display()),
            }
        }
    }
    Ok(stats)
}

/// Renames `dir` out of readers' view, then deletes it in this process.
/// `retire_dir_async` would hand the delete to a thread that dies when the
/// short-lived `admin gc` exits, leaving the bytes on disk under `.dead.*`.
/// A failure is logged and skipped so one locked dir (a Windows mapping, a
/// concurrent delete) never stops the rest of the sweep.
fn retire_and_remove(dir: &Path) -> bool {
    match ecp_core::registry::retire_dir(dir) {
        Ok(Some(retired)) => remove_retired(&retired),
        Ok(None) => false,
        Err(e) => {
            eprintln!("gc: retire {}: {e}", dir.display());
            false
        }
    }
}

fn remove_retired(dir: &Path) -> bool {
    match fs::remove_dir_all(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => {
            eprintln!("gc: remove {}: {e}", dir.display());
            false
        }
    }
}

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
        if !is_retired_dir(&name) {
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

    /// The two shapes had a predicate each, and a session retired through
    /// `retire_dir_async` matched neither the session one nor the sweep, so it
    /// stayed on disk for ever. Names are built here rather than copied as
    /// literals, so a change to `retire_dir` reaches this test.
    #[test]
    fn is_retired_dir_accepts_both_retired_shapes_and_no_live_name() {
        let tmp = tempfile::tempdir().unwrap();
        let live = tmp.path().join("sid_live");
        std::fs::create_dir_all(live.join("inner")).unwrap();
        let retired = ecp_core::registry::retire_dir(&live)
            .unwrap()
            .expect("retire_dir must move a directory that exists");
        let retired_name = retired.file_name().unwrap().to_str().unwrap();
        assert!(
            is_retired_dir(retired_name),
            "gc must sweep what retire_dir writes: {retired_name}"
        );

        // The shape `sweep_sessions` writes itself.
        assert!(is_retired_dir("sid.dead.1730000000"));
        assert!(is_retired_dir("sid.dead"));

        assert!(!is_retired_dir("sid_live"));
        assert!(!is_retired_dir("sid.dead.notanumber"));
    }
}

#[cfg(test)]
mod subtree_build_tests {
    use super::*;
    use ecp_core::registry::{EmbeddingStatus, SourceType};

    fn publish(commits: &Path, name: &str, built_from_worktree: &Path) -> std::path::PathBuf {
        publish_sha(commits, name, built_from_worktree, &"0".repeat(40))
    }

    fn publish_sha(
        commits: &Path,
        name: &str,
        built_from_worktree: &Path,
        sha: &str,
    ) -> std::path::PathBuf {
        let dir = commits.join(name);
        fs::create_dir_all(&dir).unwrap();
        let meta = CommitBuildMeta {
            version: 1,
            sha: sha.into(),
            source_type: SourceType::Branch,
            source_id: None,
            built_from_worktree: built_from_worktree.to_string_lossy().into(),
            built_at: String::new(),
            parent_sha: None,
            node_count: 0,
            embedding_status: EmbeddingStatus::None,
            refs_at_build: vec![],
            refs_seen_since: vec![],
            builder_fingerprint: None,
            binary_commit_sha: None,
        };
        CommitBuildMeta::write_atomic(&dir.join("meta.json"), &meta).unwrap();
        dir
    }

    #[test]
    fn test_sweep_subtree_builds_retires_only_slots_from_a_live_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("repo");
        let subtree = worktree.join("crates").join("cli");
        fs::create_dir_all(&subtree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        let repo_root = tmp.path().join("index");
        let commits = repo_root.join("commits");
        let from_subtree = publish(&commits, "branch_a__1111", &subtree);
        let from_root = publish(&commits, "branch_b__2222", &worktree);
        let from_gone = publish(&commits, "branch_c__3333", &tmp.path().join("gone"));
        let plain = tmp.path().join("plain-source");
        fs::create_dir_all(&plain).unwrap();
        let from_non_git = publish_sha(
            &commits,
            "branch_d__4444",
            &plain,
            &crate::build::orchestrator::path_bound_sha(&plain).unwrap(),
        );

        let retired = publish(&commits, "branch_e__5555.dead.1", &subtree);

        let stats = sweep_subtree_builds(&repo_root).unwrap();

        assert_eq!((stats.marked, stats.removed), (1, 1));
        assert!(
            retired.exists(),
            "an already retired dir is left to its sweeper"
        );
        assert!(!from_subtree.exists(), "subtree slot must be retired");
        assert!(from_root.exists(), "root slot stays");
        assert!(from_gone.exists(), "a slot whose worktree is gone stays");
        assert!(
            from_non_git.exists(),
            "a non-git source tree keyed by its path stays"
        );
    }
}
