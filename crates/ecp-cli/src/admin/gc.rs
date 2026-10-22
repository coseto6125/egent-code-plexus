//! Garbage collection: generation convergence, retired-dir sweeps, stale parse
//! caches, and commit graphs no live ref or session names.
//!
//! Covered by `tests/gc.rs` integration tests.

use crate::git::safe_exec;
use ecp_core::registry::{CommitBuildMeta, CommitDirName};
use ecp_core::session::{DirtyFiles, SessionMeta};
use rustc_hash::{FxHashMap, FxHashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

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

/// True when `last_use` lies at least `secs` before `now`. An unknown time or
/// one in the future is not idle, so a stat quirk never deletes.
fn idle_for(last_use: Option<SystemTime>, now: SystemTime, secs: u64) -> bool {
    last_use
        .and_then(|t| now.duration_since(t).ok())
        .is_some_and(|age| age.as_secs() >= secs)
}

/// The grace before gc reclaims what another installed ecp version may still
/// use (its parse cache, its older-format graphs) or what a short-lived use
/// such as `impact --baseline <old ref>` built. A swept item is rebuilt on
/// demand.
const IDLE_GRACE_SECS: u64 = 7 * 86_400;

/// When a commit dir was last used: the newer of the dir's mtime (a build or
/// an attach wrote into it) and its `graph.bin`'s atime (a query mapped it).
/// Linux `relatime` refreshes atime at most once a day, often enough for
/// [`IDLE_GRACE_SECS`]; on a `noatime` mount the atime stays at the build, so
/// there the dir's mtime alone counts.
fn commit_dir_last_use(dir: &Path, dir_meta: &fs::Metadata) -> Option<SystemTime> {
    let read = fs::metadata(dir.join("graph.bin"))
        .and_then(|m| m.accessed())
        .ok();
    dir_meta.modified().ok().max(read)
}

/// `would_remove` lists what a dry run found and left on disk; `removed`
/// counts only what a real run deleted.
#[derive(Default)]
pub struct SweepStats {
    pub marked: usize,
    pub removed: usize,
    pub would_remove: Vec<std::path::PathBuf>,
}

impl SweepStats {
    /// Removes `path` through `remove` and counts it, or, in a dry run, lists
    /// it and touches nothing.
    fn dispose(&mut self, path: &Path, dry_run: bool, remove: impl FnOnce() -> bool) {
        if dry_run {
            self.would_remove.push(path.to_path_buf());
        } else {
            self.removed += usize::from(remove());
        }
    }

    pub fn merge(&mut self, other: SweepStats) {
        self.marked += other.marked;
        self.removed += other.removed;
        self.would_remove.extend(other.would_remove);
    }
}

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

/// Adds the base SHA of every session touched within [`SESSION_IDLE_HOURS`].
/// Returns false when the scan may have missed one: an unreadable sessions
/// dir, entry or meta file (not absent, not corrupt) leaves a live base
/// unseen, and a caller that deletes on this set must not trust it then.
fn insert_active_session_bases(repo_root: &Path, set: &mut FxHashSet<String>) -> bool {
    let it = match fs::read_dir(repo_root.join("sessions")) {
        Ok(it) => it,
        Err(e) => return e.kind() == io::ErrorKind::NotFound,
    };
    let mut complete = true;
    for entry in it {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if is_retired_dir(&s) || s.contains(".stale-") {
            continue;
        }
        let sm = match SessionMeta::read(&entry.path().join("session_meta.json")) {
            Ok(sm) => sm,
            // Absent: a dir mid-creation or mid-removal. `Other`: unparsable
            // JSON, which no retry will fix. Not a directory: a stray file
            // such as `.DS_Store`. None of them names a live base.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::Other | io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(_) => {
                complete = false;
                continue;
            }
        };
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&sm.last_touched) {
            let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
            if age.num_hours() > SESSION_IDLE_HOURS {
                continue;
            }
        }
        set.insert(sm.base_sha);
    }
    complete
}

/// Every SHA a ref or a worktree HEAD of the repo at `common_dir` names, or
/// `None` when git cannot answer. Only a complete answer may justify a
/// delete: an empty set from a failed git call would mark every graph dead.
///
/// Also live: the commit an annotated tag points at (the tag's own object id
/// is not a commit dir), the parents of each worktree HEAD (warm-attach
/// serves a HEAD that has no graph yet from its parent's graph), and the
/// bases of live sessions.
fn live_commit_shas(repo_root: &Path, common_dir: &Path) -> Option<FxHashSet<String>> {
    let git = |args: &[&str]| {
        let mut cmd = safe_exec::git();
        // An inherited GIT_DIR / GIT_COMMON_DIR overrides `--git-dir` for
        // some subcommands (`worktree list` answered for another repo).
        for var in [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
        ] {
            cmd.env_remove(var);
        }
        let out = cmd
            .arg("--git-dir")
            .arg(common_dir)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let is_sha = |s: &str| {
        s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()) && s.bytes().any(|b| b != b'0')
    };
    let mut set: FxHashSet<String> =
        git(&["for-each-ref", "--format=%(objectname) %(*objectname)"])?
            .split_whitespace()
            .filter(|s| is_sha(s))
            .map(str::to_owned)
            .collect();
    // A detached worktree HEAD is not a ref.
    let heads: Vec<String> = git(&["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|line| line.strip_prefix("HEAD "))
        .map(str::trim)
        .filter(|s| is_sha(s))
        .map(str::to_owned)
        .collect();
    if !heads.is_empty() {
        let mut args = vec!["rev-list", "--no-walk", "--parents"];
        args.extend(heads.iter().map(String::as_str));
        set.extend(
            git(&args)?
                .split_whitespace()
                .filter(|s| is_sha(s))
                .map(str::to_owned),
        );
    }
    set.extend(heads);
    insert_active_session_bases(repo_root, &mut set).then_some(set)
}

/// Retires commit graphs whose SHA is not live (see [`live_commit_shas`]),
/// that were built more than [`IDLE_GRACE_SECS`] ago and that were not used
/// within it (see [`commit_dir_last_use`]). A merged branch or a deleted
/// worktree otherwise leaves every graph it built behind. Removes nothing
/// when the repo's git dir is unknown or git fails.
pub fn sweep_unreachable_commits(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let Ok(it) = fs::read_dir(repo_root.join("commits")) else {
        return Ok(stats);
    };
    let now = SystemTime::now();
    // Pass 1, stat only: a dir used within the grace period is kept, so a
    // repo with no older dir pays no git call. Not only an optimisation: an
    // attach back-fills sidecars into a dir built long ago, and that recent
    // write keeps the graph in use.
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
            stats.dispose(&dir, dry_run, || remove_retired(&dir));
            continue;
        }
        if name.starts_with('.')
            || crate::commit_lookup::is_recovery_dir_name(&name)
            || !idle_for(commit_dir_last_use(&dir, &meta), now, IDLE_GRACE_SECS)
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
    let Some(live) = live_commit_shas(repo_root, Path::new(&repo_meta.common_dir)) else {
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
            u64::try_from(
                chrono::Utc::now()
                    .signed_duration_since(built.with_timezone(&chrono::Utc))
                    .num_seconds(),
            )
            .is_ok_and(|age| age >= IDLE_GRACE_SECS)
        });
        if !old_enough {
            continue;
        }
        stats.marked += 1;
        stats.dispose(&dir, dry_run, || retire_and_remove(&dir));
    }
    Ok(stats)
}

pub fn sweep_sessions(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let sessions_dir = repo_root.join("sessions");
    let mut stats = SweepStats::default();
    let Ok(it) = fs::read_dir(&sessions_dir) else {
        return Ok(stats);
    };
    for entry in it.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if is_retired_dir(&name) || name.contains(".stale-") {
            if dry_run {
                stats.would_remove.push(path);
            } else {
                fs::remove_dir_all(&path)?;
                stats.removed += 1;
            }
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
            if !dry_run {
                let ts = chrono::Utc::now().timestamp();
                let dead = path.with_extension(format!("dead.{ts}"));
                fs::rename(&path, &dead)?;
            }
            stats.marked += 1;
        }
    }
    Ok(stats)
}

/// Retire commit dirs whose recorded worktree still exists but is not a
/// worktree root (`orchestrator::built_below_worktree_root`): they were
/// published from a subdirectory and hold that subtree's graph only. A
/// recorded worktree that is gone is left alone; the query path rebuilds such
/// a slot on first use.
pub fn sweep_subtree_builds(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let Ok(it) = fs::read_dir(repo_root.join("commits")) else {
        return Ok(stats);
    };
    for entry in it.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !dir.is_dir()
            || name.starts_with('.')
            || crate::commit_lookup::is_recovery_dir_name(&name)
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
            stats.dispose(&dir, dry_run, || retire_and_remove(&dir));
        }
    }
    Ok(stats)
}

/// Removes `parse_cache/<fp>/` dirs of other fingerprints that no writer
/// touched for [`IDLE_GRACE_SECS`], plus `.dead.*` leftovers of an earlier
/// retire whose background delete did not finish. Every release and every
/// `BUILDER_FINGERPRINT` bump opens a new dir and never reads the old one
/// again; the grace keeps a second installed ecp version's live cache from
/// being evicted on each gc, which would make the two binaries re-parse in
/// turn.
pub fn sweep_stale_parse_caches(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let Ok(it) = fs::read_dir(repo_root.join("parse_cache")) else {
        return Ok(stats);
    };
    let current = crate::parse_cache::ParseCache::current_dir_name();
    let now = SystemTime::now();
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
            stats.dispose(&dir, dry_run, || remove_retired(&dir));
            continue;
        }
        // Last use: the newer of the dir's mtime (an entry written) and the
        // stamp `ParseCache::open` rewrites (a run that only read). A binary
        // older than the stamp leaves only the first; its read-only reuse can
        // then be swept, which costs one re-parse, never a wrong answer.
        let stamp = fs::metadata(dir.join(crate::parse_cache::LAST_USED_STAMP))
            .and_then(|m| m.modified())
            .ok();
        let last_use = meta.modified().ok().max(stamp);
        if name == current || !idle_for(last_use, now, IDLE_GRACE_SECS) {
            continue;
        }
        stats.marked += 1;
        stats.dispose(&dir, dry_run, || retire_and_remove(&dir));
    }
    Ok(stats)
}

/// Removes commit dirs whose `graph.bin` is in an older
/// `GRAPH_FORMAT_VERSION` layout, which this binary never reads again: it
/// rebuilds such a slot. Kept while used within [`IDLE_GRACE_SECS`] (see
/// [`commit_dir_last_use`]), because a second installed ecp version may still
/// read them.
pub fn sweep_old_format_commits(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let Ok(it) = fs::read_dir(repo_root.join("commits")) else {
        return Ok(stats);
    };
    let now = SystemTime::now();
    for entry in it.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !meta.is_dir()
            || name.starts_with('.')
            || crate::commit_lookup::is_recovery_dir_name(&name)
            || is_retired_dir(&name)
        {
            continue;
        }
        let dir = entry.path();
        let graph = dir.join("graph.bin");
        if !idle_for(commit_dir_last_use(&dir, &meta), now, IDLE_GRACE_SECS)
            || !graph.is_file()
            || !older_graph_format(&graph)
        {
            continue;
        }
        stats.marked += 1;
        stats.dispose(&dir, dry_run, || retire_and_remove(&dir));
    }
    Ok(stats)
}

/// Whether `graph` is proven to be in an older layout than this binary
/// reads: its version sidecar says so, or, with no sidecar (a binary before
/// the sidecar, or a lost detached write), its header records an older
/// version. A newer version, or a header that cannot be read, is kept: a
/// failed check is not proof of age.
fn older_graph_format(graph: &Path) -> bool {
    crate::auto_ensure::read_compatible_version_sidecar(graph)
        .or_else(|| graph_format_version(graph))
        .is_some_and(|version| version < ecp_core::graph::GRAPH_FORMAT_VERSION)
}

/// How many bytes at the end of a `graph.bin` hold its root object. The v13
/// root is 240 bytes and v14 adds two vectors to it; a root that outgrows this reads as unknown, so kept.
const GRAPH_ROOT_TAIL_BYTES: u64 = 4096;

/// The format version a `graph.bin` header records, or `None` when no header
/// is found. Validation through this binary's archived type cannot tell an
/// older layout from a newer or a corrupt one; but every version leads its
/// root with `magic` then `version`, and rkyv writes the root last, so the
/// last magic in the file's tail is the root's.
fn graph_format_version(graph: &Path) -> Option<u32> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(graph).ok()?;
    let meta = file.metadata().ok()?;
    file.seek(SeekFrom::Start(
        meta.len().saturating_sub(GRAPH_ROOT_TAIL_BYTES),
    ))
    .ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    // This read would refresh the atime that `commit_dir_last_use` reads as
    // a query's use, and keep an idle graph alive for ever.
    if let Ok(accessed) = meta.accessed() {
        let _ = file.set_times(fs::FileTimes::new().set_accessed(accessed));
    }
    let magic = ecp_core::graph::GRAPH_MAGIC;
    let at = tail.windows(magic.len()).rposition(|w| w == magic)? + magic.len();
    Some(u32::from_le_bytes(tail.get(at..at + 4)?.try_into().ok()?))
}

/// A fragment bin younger than this may belong to a writer that renamed it
/// in and has not yet committed the manifest naming it.
const ORPHAN_FRAGMENT_IDLE_SECS: u64 = 3600;

/// Removes the fragment bins of each session that its `dirty_files.json`
/// no longer names: bins are content-addressed, so every save of a dirty
/// file writes a new one, and a manifest another builder wrote is replaced
/// whole. Leftover `.tmp` writes go too. A session whose manifest cannot be
/// read keeps its bins, and so does one whose writer holds its overlay lock.
pub fn sweep_stale_fragments(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let Ok(sessions) = fs::read_dir(repo_root.join("sessions")) else {
        return Ok(stats);
    };
    let now = SystemTime::now();
    for session in sessions.flatten() {
        let name = session.file_name().to_string_lossy().to_string();
        if is_retired_dir(&name) || name.contains(".stale-") {
            continue;
        }
        let dir = session.path();
        let overlay = dir.join("graph_overlay");
        if !overlay.is_dir() {
            continue;
        }
        // Held from the manifest read to the last unlink: a bin's name is its
        // content hash, so a writer can re-publish a bin judged orphan here
        // under the same name, and the unlink would then delete the new one.
        let Ok(_publish) = ecp_core::registry::FileLock::try_exclusive(
            &crate::session::overlay_writer::overlay_lock_path(&dir),
        ) else {
            continue;
        };
        let Ok(bins) = fs::read_dir(&overlay) else {
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
            if !orphan
                || !meta.is_file()
                || !idle_for(meta.modified().ok(), now, ORPHAN_FRAGMENT_IDLE_SECS)
            {
                continue;
            }
            stats.marked += 1;
            let path = bin.path();
            stats.dispose(&path, dry_run, || match fs::remove_file(&path) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("gc: remove {}: {e}", path.display());
                    false
                }
            });
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

/// Converge same-SHA generation dirs under `<repo_root>/commits/`: for each SHA,
/// keep only the dir with the greatest `Generation` (a base dir with no `.gen`
/// suffix has `generation == None`, ordering below any `Some(_)`), remove the
/// rest. Same SHA → identical graph (ingest is idempotent), so older generations
/// are pure waste. Skips dirs whose mtime is < 10s old or that have a sibling
/// `.building` marker (another session may be mid-ingest). Reuses
/// `CommitDirName::parse` rather than hand-rolling the name grammar.
pub fn sweep_stale_generations(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    sweep_generations_keeping(repo_root, dry_run, None)
}

/// [`sweep_stale_generations`] for a caller that just published `published`.
/// That dir is settled by construction, so the 10 s in-flight guard does not
/// apply to it (without this, a fresh publish hides itself and its
/// predecessor is never paired with it), and it is never removed.
pub fn sweep_generations_keeping(
    repo_root: &Path,
    dry_run: bool,
    published: Option<&Path>,
) -> io::Result<SweepStats> {
    let commits = repo_root.join("commits");
    let mut stats = SweepStats::default();
    let Ok(it) = fs::read_dir(&commits) else {
        return Ok(stats);
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
        if is_in_flight(&meta, now) && published != Some(entry.path().as_path()) {
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
            if i == keep_idx || published == Some(path.as_path()) {
                continue;
            }
            stats.dispose(path, dry_run, || match fs::remove_dir_all(path) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!(
                        "gc: failed to remove stale generation {}: {e}",
                        path.display()
                    );
                    false
                }
            });
        }
    }

    Ok(stats)
}

/// Remove retired dirs (`<name>.dead.<pid>.<n>.<ts>`) directly under
/// `parent` left behind when `fs_safe::retire_dir_async`'s background delete
/// thread died with the process before finishing: repo dirs under `~/.ecp`,
/// and a repo root's `parse_cache` that `admin index --force` retires whole.
/// Already marked dead → removal is unconditional.
pub fn sweep_retired_dirs(parent: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let Ok(it) = fs::read_dir(parent) else {
        return Ok(stats);
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
        stats.dispose(&path, dry_run, || match fs::remove_dir_all(&path) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("gc: failed to remove retired dir {}: {e}", path.display());
                false
            }
        });
    }
    Ok(stats)
}

/// Remove orphaned atomic-write temp siblings (`<name>.<pid>.<n>.tmp`) left in
/// `home_ecp` when a writer's `tmp → fsync → rename` (registry/io.rs) was
/// interrupted before the rename. The io.rs doc comment promises these "can be
/// swept by cleanup tools" — this is that tool. Settled files only (see
/// [`is_in_flight`]), so a live writer mid-`rename` is never touched.
pub fn sweep_orphan_tmp(home_ecp: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let Ok(it) = fs::read_dir(home_ecp) else {
        return Ok(stats);
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
        let path = entry.path();
        stats.dispose(&path, dry_run, || match fs::remove_file(&path) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("gc: failed to remove orphan tmp {}: {e}", path.display());
                false
            }
        });
    }
    Ok(stats)
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

        let stats = sweep_orphan_tmp(home, false).expect("sweep");

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

        let stats = sweep_orphan_tmp(home, false).expect("sweep");
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

    /// A branch such as `feat/x.building-y` puts `.building` before the `__`;
    /// only a suffix after it marks an in-progress build.
    #[test]
    fn test_sweep_subtree_builds_branch_named_building_retired() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("repo");
        let subtree = worktree.join("crates").join("cli");
        fs::create_dir_all(&subtree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        let repo_root = tmp.path().join("index");
        let commits = repo_root.join("commits");
        let named = publish(&commits, "branch_feat_x.building-y__6666", &subtree);
        let building = publish(&commits, "branch_feat_z__7777.building", &subtree);

        let stats = sweep_subtree_builds(&repo_root).unwrap();

        assert_eq!((stats.marked, stats.removed), (1, 1));
        assert!(
            !named.exists(),
            "a branch named *.building* was never swept"
        );
        assert!(building.exists(), "an in-progress build was swept");
    }
}
