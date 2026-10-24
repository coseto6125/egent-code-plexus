//! Garbage collection: generation convergence, retired-dir sweeps, stale parse
//! caches, and commit graphs no live ref or session names.
//!
//! Covered by `tests/gc.rs` integration tests.

use crate::commit_lookup::{
    legacy_staging_last_write, sha_lock_path, try_sha_lock, CommitDir, CommitDirListing,
    StagingShape,
};
use crate::git::safe_exec;
use ecp_core::registry::{CommitBuildMeta, CommitDirName, FileLock};
use ecp_core::session::{DirtyFiles, SessionMeta};
use rustc_hash::{FxHashMap, FxHashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
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
        if session_is_idle(&sm) {
            continue;
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
    sweep_unreachable_commits_with(repo_root, dry_run, &CommitsFs::DURABLE)
}

/// [`sweep_unreachable_commits`] with the listing and the directory fsync
/// passed in.
pub(crate) fn sweep_unreachable_commits_with(
    repo_root: &Path,
    dry_run: bool,
    fs_ops: &CommitsFs,
) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let commits = repo_root.join("commits");
    let Ok(listing) = (fs_ops.read_listing)(&commits) else {
        return Ok(stats);
    };
    // Leftovers of an earlier retire whose delete did not finish.
    for name in &listing.retired {
        let dir = commits.join(name);
        stats.dispose(&dir, dry_run, || remove_retired(&dir));
    }
    let now = SystemTime::now();
    // An attach back-fills sidecars into a dir built long ago, and that
    // recent write keeps the graph in use.
    let idle = |dir: &CommitDir| {
        fs::metadata(&dir.path)
            .is_ok_and(|m| idle_for(commit_dir_last_use(&dir.path, &m), now, IDLE_GRACE_SECS))
    };
    // Pass 1, stat only: a dir used within the grace period is kept, so a
    // repo with no older dir pays no git call.
    let idle_shas: FxHashSet<[u8; 20]> = listing
        .dirs
        .iter()
        .filter(|d| idle(d))
        .map(|d| d.parsed.sha)
        .collect();
    if idle_shas.is_empty() {
        return Ok(stats);
    }
    let Ok(repo_meta) = ecp_core::registry::RepoMeta::read(&repo_root.join("meta.json")) else {
        return Ok(stats);
    };
    let Some(live) = live_commit_shas(repo_root, Path::new(&repo_meta.common_dir)) else {
        return Ok(stats);
    };
    let unreachable = |dir: &CommitDir| idle(dir) && built_before_grace(&dir.path);
    for sha in idle_shas {
        if live.contains(&hex::encode(sha)) {
            continue;
        }
        // A builder of this SHA may be about to attach to a dir or publish
        // beside it; under the lock each dir is judged again as it is now.
        let retired =
            retire_under_sha_lock(&commits, &sha, dry_run, &listing, fs_ops, &mut stats, |l| {
                l.same_sha(&sha)
                    .filter(|d| unreachable(d))
                    .map(|d| d.path.clone())
                    .collect()
            });
        stats.marked += retired;
    }
    Ok(stats)
}

/// True when the dir's `meta.json` records a build older than
/// [`IDLE_GRACE_SECS`]. An unreadable meta or time is not old enough.
fn built_before_grace(dir: &Path) -> bool {
    let Ok(cm) = CommitBuildMeta::read(&dir.join("meta.json")) else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(&cm.built_at).is_ok_and(|built| {
        u64::try_from(
            chrono::Utc::now()
                .signed_duration_since(built.with_timezone(&chrono::Utc))
                .num_seconds(),
        )
        .is_ok_and(|age| age >= IDLE_GRACE_SECS)
    })
}

fn session_is_idle(sm: &SessionMeta) -> bool {
    chrono::DateTime::parse_from_rfc3339(&sm.last_touched).is_ok_and(|t| {
        chrono::Utc::now()
            .signed_duration_since(t.with_timezone(&chrono::Utc))
            .num_hours()
            > SESSION_IDLE_HOURS
    })
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
        if session_is_idle(&sm) {
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
    let is_subtree_build = |dir: &Path| {
        let Ok(meta) = CommitBuildMeta::read(&dir.join("meta.json")) else {
            return false;
        };
        let built = Path::new(&meta.built_from_worktree);
        // `Path::new("")` as the root in use: no live root is exempt here.
        built.is_dir()
            && crate::build::orchestrator::built_below_worktree_root(
                built,
                &meta.sha,
                Path::new(""),
            )
    };
    sweep_commit_dirs_where(repo_root, dry_run, &is_subtree_build)
}

/// Retires every dir under `commits/` that `doomed` accepts, each SHA's dirs
/// through [`retire_under_sha_lock`], which judges them again under the lock.
/// A dir whose name holds no SHA has no lock: no builder publishes, attaches
/// to or supersedes such a name, so it is retired directly.
fn sweep_commit_dirs_where(
    repo_root: &Path,
    dry_run: bool,
    doomed: &dyn Fn(&Path) -> bool,
) -> io::Result<SweepStats> {
    let mut stats = SweepStats::default();
    let commits = repo_root.join("commits");
    let Ok(listing) = CommitDirListing::read(&commits) else {
        return Ok(stats);
    };
    for name in listing.unparsed.iter().filter(|n| !n.starts_with('.')) {
        let dir = commits.join(name);
        if doomed(dir.as_path()) {
            stats.marked += 1;
            stats.dispose(&dir, dry_run, || retire_and_remove(&dir));
        }
    }
    let shas: FxHashSet<[u8; 20]> = listing
        .dirs
        .iter()
        .filter(|d| doomed(d.path.as_path()))
        .map(|d| d.parsed.sha)
        .collect();
    for sha in shas {
        let retired = retire_under_sha_lock(
            &commits,
            &sha,
            dry_run,
            &listing,
            &CommitsFs::DURABLE,
            &mut stats,
            |l| {
                l.same_sha(&sha)
                    .filter(|d| doomed(d.path.as_path()))
                    .map(|d| d.path.clone())
                    .collect()
            },
        );
        stats.marked += retired;
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
    let now = SystemTime::now();
    let is_old_format = |dir: &Path| {
        let graph = dir.join("graph.bin");
        fs::metadata(dir)
            .is_ok_and(|m| idle_for(commit_dir_last_use(dir, &m), now, IDLE_GRACE_SECS))
            && graph.is_file()
            && older_graph_format(&graph)
    };
    sweep_commit_dirs_where(repo_root, dry_run, &is_old_format)
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
        // A real GC retires idle sessions before this sweep. A dry run must
        // leave their fragments out of its deletion report too.
        if SessionMeta::read(&dir.join("session_meta.json")).is_ok_and(|sm| session_is_idle(&sm)) {
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

/// Converge same-SHA generation dirs under `<repo_root>/commits/`: per SHA,
/// keep the dir readers pick ([`CommitDirListing::current`]) and retire the
/// rest. Same SHA → identical graph (ingest is idempotent), so older
/// generations are pure waste. Skips dirs younger than 10 s, a SHA whose lock
/// another process holds, and a SHA an older ecp may be building (see
/// [`legacy_build_active`]).
pub fn sweep_stale_generations(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    sweep_generations_keeping(repo_root, dry_run, None)
}

/// [`sweep_stale_generations`] for a caller that just published `published`.
/// That dir is settled by construction, and so is every lower generation of
/// its SHA, which it supersedes: the 10 s in-flight guard applies to neither
/// (without this, a fresh publish hides itself, and a forced build seconds
/// after the last one keeps its predecessor). The published dir is never
/// removed.
pub fn sweep_generations_keeping(
    repo_root: &Path,
    dry_run: bool,
    published: Option<&Path>,
) -> io::Result<SweepStats> {
    sweep_generations_with(
        repo_root,
        dry_run,
        published,
        &CommitDirListing::read,
        &ecp_core::registry::sync_dir,
    )
}

/// [`sweep_generations_keeping`] with the listing and the directory fsync
/// passed in, so a test can tear the listing or fail the fsync.
pub(crate) fn sweep_generations_with(
    repo_root: &Path,
    dry_run: bool,
    published: Option<&Path>,
    read_listing: &dyn Fn(&Path) -> io::Result<CommitDirListing>,
    sync_dir: &dyn Fn(&Path) -> io::Result<()>,
) -> io::Result<SweepStats> {
    let fs_ops = CommitsFs {
        read_listing,
        sync_dir,
    };
    let commits = repo_root.join("commits");
    let mut stats = SweepStats::default();
    let Ok(listing) = read_listing(&commits) else {
        return Ok(stats);
    };
    // A missed entry may be the current dir of a SHA; retiring what then
    // looks older would delete the graph readers use.
    if !listing.complete {
        eprintln!(
            "gc: {} listing incomplete; generation sweep skipped",
            commits.display()
        );
        return Ok(stats);
    }
    let now = SystemTime::now();
    let published_name = published
        .and_then(Path::file_name)
        .and_then(|n| CommitDirName::parse(&n.to_string_lossy()).ok());
    let settled = |dir: &CommitDir| {
        let superseded = published_name
            .as_ref()
            .is_some_and(|p| p.sha == dir.parsed.sha && dir.parsed.generation <= p.generation);
        superseded || fs::symlink_metadata(&dir.path).is_ok_and(|m| !is_in_flight(&m, now))
    };
    let mut settled_per_sha: FxHashMap<[u8; 20], usize> = FxHashMap::default();
    for dir in listing.dirs.iter().filter(|d| settled(d)) {
        *settled_per_sha.entry(dir.parsed.sha).or_default() += 1;
    }

    for (sha, settled_count) in settled_per_sha {
        if settled_count < 2 || legacy_build_active(&listing, &sha, &commits, now) {
            continue;
        }
        // Judged on the listing read under the lock: a builder may have
        // published a higher generation since the first read, and that one is
        // current now.
        retire_under_sha_lock(
            &commits,
            &sha,
            dry_run,
            &listing,
            &fs_ops,
            &mut stats,
            |l| {
                let Some(current) = l.current(&sha) else {
                    return Vec::new();
                };
                l.same_sha(&sha)
                    .filter(|d| d.name != current.name && settled(d))
                    .map(|d| d.path.clone())
                    .filter(|path| published != Some(path.as_path()))
                    .collect()
            },
        );
    }

    Ok(stats)
}

/// Retires new-style staging dirs (`<base>.building.<pid>.<n>`) whose SHA
/// lock is free, whatever else exists for the SHA. A builder keeps such a
/// dir only while it holds that lock, so a free lock proves the dir
/// abandoned (a crash, a kill); the PID in the name proves nothing, PIDs are
/// reused. Old-style `<base>.building/` dirs are never touched: an older ecp
/// may still own one, and `ecp admin doctor` reports them instead.
pub fn sweep_abandoned_staging(repo_root: &Path, dry_run: bool) -> io::Result<SweepStats> {
    let commits = repo_root.join("commits");
    let mut stats = SweepStats::default();
    let Ok(listing) = CommitDirListing::read(&commits) else {
        return Ok(stats);
    };
    let shas: FxHashSet<[u8; 20]> = listing
        .staging
        .iter()
        .filter(|s| matches!(s.shape, StagingShape::PerAttempt { .. }))
        .map(|s| s.sha)
        .collect();
    for sha in shas {
        let retired = retire_under_sha_lock(
            &commits,
            &sha,
            dry_run,
            &listing,
            &CommitsFs::DURABLE,
            &mut stats,
            |l| {
                l.staging
                    .iter()
                    .filter(|s| s.sha == sha && matches!(s.shape, StagingShape::PerAttempt { .. }))
                    .map(|s| commits.join(&s.name))
                    .collect()
            },
        );
        stats.marked += retired;
    }
    Ok(stats)
}

/// How gc reads `commits/` and fsyncs it; a test passes its own.
pub(crate) struct CommitsFs<'a> {
    pub read_listing: &'a dyn Fn(&Path) -> io::Result<CommitDirListing>,
    pub sync_dir: &'a dyn Fn(&Path) -> io::Result<()>,
}

impl CommitsFs<'static> {
    pub(crate) const DURABLE: Self = Self {
        read_listing: &CommitDirListing::read,
        sync_dir: &ecp_core::registry::sync_dir,
    };
}

/// The one way gc retires dirs of a SHA, for every sweep of `commits/`.
///
/// A real run takes the SHA's stripe lock, try only: busy or an error skips
/// the SHA in this run. Under the lock it reads `commits/` again and lets
/// `victims` judge that complete listing, so a dir a builder published or
/// used since the first read is judged as it is now. Then it fsyncs
/// `commits/`: a builder that died after its rename and before its own fsync
/// left a publish that a power loss can undo, and the fsync makes it durable
/// before anything it supersedes goes. Then it retires each victim.
///
/// A dry run judges `listing`, only probes the lock, and writes nothing.
/// Returns how many dirs it retired or listed.
fn retire_under_sha_lock(
    commits: &Path,
    sha: &[u8; 20],
    dry_run: bool,
    listing: &CommitDirListing,
    fs_ops: &CommitsFs,
    stats: &mut SweepStats,
    victims: impl Fn(&CommitDirListing) -> Vec<PathBuf>,
) -> usize {
    let _lock = match sweep_sha_lock(commits, sha, dry_run) {
        ShaLock::Busy => return 0,
        ShaLock::DryRun => {
            let victims = victims(listing);
            for path in &victims {
                stats.dispose(path, true, || false);
            }
            return victims.len();
        }
        ShaLock::Held(lock) => lock,
    };
    let fresh = match (fs_ops.read_listing)(commits) {
        Ok(fresh) if fresh.complete => fresh,
        _ => return 0,
    };
    let victims = victims(&fresh);
    if victims.is_empty() {
        return 0;
    }
    if let Err(e) = (fs_ops.sync_dir)(commits) {
        eprintln!(
            "gc: fsync {}: {e}; dirs of {} kept",
            commits.display(),
            hex::encode(sha)
        );
        return 0;
    }
    for path in &victims {
        stats.dispose(path, false, || retire_and_remove(path));
    }
    victims.len()
}

/// gc's hold on a SHA's stripe lock.
pub(crate) enum ShaLock {
    /// A real run holds the lock for the sweep of that SHA.
    Held(FileLock),
    /// A dry run found the lock free or absent. It created nothing: no
    /// `.locks` dir, no lock file, no owner sidecar.
    DryRun,
    /// Another process (or another descriptor of this one) holds the stripe,
    /// or the lock could not be taken: the SHA is skipped in this run.
    Busy,
}

/// The SHA lock for a gc sweep, try only. A real run names itself the
/// holder (`<pid> gc <sha40>`), so a builder that waits for the stripe
/// reports gc. A dry run only probes, so it judges every SHA as a real run
/// would and leaves nothing on disk.
pub(crate) fn sweep_sha_lock(commits: &Path, sha: &[u8; 20], dry_run: bool) -> ShaLock {
    if dry_run {
        return match FileLock::try_existing(&sha_lock_path(commits, sha)) {
            Ok(_) => ShaLock::DryRun,
            Err(_) => ShaLock::Busy,
        };
    }
    match try_sha_lock(commits, sha, "gc") {
        Ok(lock) => ShaLock::Held(lock),
        Err(_) => ShaLock::Busy,
    }
}

/// True while an older ecp may be building the SHA: an old-style
/// `<base>.building/` of it was written within [`IDLE_GRACE_SECS`] (see
/// [`legacy_staging_last_write`]), or its age cannot be read. Its
/// `.build.lock` is not probed: an older ecp that tries that lock while a
/// probe holds it, even for an instant, fails its build after 5 s.
fn legacy_build_active(
    listing: &CommitDirListing,
    sha: &[u8; 20],
    commits: &Path,
    now: SystemTime,
) -> bool {
    listing
        .staging
        .iter()
        .filter(|s| s.sha == *sha && s.shape == StagingShape::Legacy)
        .any(|s| {
            !idle_for(
                legacy_staging_last_write(&commits.join(&s.name)),
                now,
                IDLE_GRACE_SECS,
            )
        })
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

        let stats = sweep_subtree_builds(&repo_root, false).unwrap();

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

        let stats = sweep_subtree_builds(&repo_root, false).unwrap();

        assert_eq!((stats.marked, stats.removed), (1, 1));
        assert!(
            !named.exists(),
            "a branch named *.building* was never swept"
        );
        assert!(building.exists(), "an in-progress build was swept");
    }
}

#[cfg(test)]
mod sha_lock_sweep_tests {
    use super::*;
    use crate::commit_lookup::staging_dir_name;
    use std::cell::Cell;
    use std::time::Duration;

    const SHA: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";

    fn sha() -> [u8; 20] {
        crate::build::orchestrator::parse_sha(SHA).unwrap()
    }

    /// A dir past the 10 s in-flight guard.
    fn settled_dir(commits: &Path, name: &str) -> PathBuf {
        let dir = commits.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("graph.bin"), b"g").unwrap();
        let old = filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(60));
        filetime::set_file_mtime(&dir, old).unwrap();
        dir
    }

    fn two_generations(root: &Path) -> (PathBuf, PathBuf) {
        let commits = root.join("commits");
        (
            settled_dir(&commits, &format!("commit__{SHA}.gen.1000.1.0")),
            settled_dir(&commits, &format!("commit__{SHA}.gen.2000.1.0")),
        )
    }

    fn lock_path(root: &Path) -> PathBuf {
        sha_lock_path(&root.join("commits"), &sha())
    }

    /// Hold `sha`'s stripe as a builder does.
    fn hold(root: &Path, sha: &[u8; 20]) -> FileLock {
        crate::build::orchestrator::acquire_sha_lock(
            &root.join("commits"),
            sha,
            Duration::ZERO,
            |_| panic!("nothing else holds the stripe"),
        )
        .unwrap()
    }

    /// Every file and dir under `root`, with each file's bytes.
    fn tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        walkdir::WalkDir::new(root)
            .sort_by_file_name()
            .into_iter()
            .map(|e| {
                let e = e.unwrap();
                let body = e.file_type().is_file().then(|| fs::read(e.path()).unwrap());
                (e.path().to_path_buf(), body)
            })
            .collect()
    }

    /// A builder holds the SHA from its last existence check through its
    /// fsync of `commits/`; gc must not retire a generation inside that span.
    #[test]
    fn test_sweep_stale_generations_sha_lock_held_skips_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = two_generations(tmp.path());
        let held = hold(tmp.path(), &sha());
        let stats = sweep_stale_generations(tmp.path(), false).unwrap();
        assert_eq!(stats.removed, 0);
        assert!(old.exists() && new.exists());
        drop(held);
        let stats = sweep_stale_generations(tmp.path(), false).unwrap();
        assert_eq!(stats.removed, 1);
        assert!(!old.exists() && new.exists());
    }

    /// F2: the newer generation's rename may not be durable yet. Without a
    /// successful fsync of `commits/`, the previous generation stays.
    #[test]
    fn test_sweep_generations_with_commits_fsync_fails_previous_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = two_generations(tmp.path());
        let fail = |_: &Path| -> io::Result<()> { Err(io::Error::other("injected")) };
        let stats = sweep_generations_with(tmp.path(), false, None, &CommitDirListing::read, &fail)
            .unwrap();
        assert_eq!(stats.removed, 0);
        assert!(old.exists() && new.exists());
    }

    /// F2: a builder died after its rename and before its fsync of
    /// `commits/`. gc's fsync runs while both generations still exist, and
    /// the previous one goes only after it.
    #[test]
    fn test_sweep_generations_with_publish_not_durable_fsync_before_retire() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = two_generations(tmp.path());
        let commits = tmp.path().join("commits");
        let both_present_at_sync = Cell::new(None);
        let record = |dir: &Path| -> io::Result<()> {
            assert_eq!(dir, commits);
            both_present_at_sync.set(Some(old.exists() && new.exists()));
            Ok(())
        };
        let stats =
            sweep_generations_with(tmp.path(), false, None, &CommitDirListing::read, &record)
                .unwrap();
        assert_eq!(both_present_at_sync.get(), Some(true));
        assert_eq!(stats.removed, 1);
        assert!(!old.exists() && new.exists());
    }

    /// A torn listing may have missed the current dir of a SHA.
    #[test]
    fn test_sweep_generations_with_incomplete_listing_removes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = two_generations(tmp.path());
        let torn = |dir: &Path| -> io::Result<CommitDirListing> {
            let mut listing = CommitDirListing::read(dir)?;
            listing.complete = false;
            Ok(listing)
        };
        let stats = sweep_generations_with(
            tmp.path(),
            false,
            None,
            &torn,
            &ecp_core::registry::sync_dir,
        )
        .unwrap();
        assert_eq!(stats.removed, 0);
        assert!(old.exists() && new.exists());
    }

    #[test]
    fn test_sweep_stale_generations_dry_run_creates_no_lock_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = two_generations(tmp.path());
        let stats = sweep_stale_generations(tmp.path(), true).unwrap();
        assert_eq!(stats.would_remove, vec![old.clone()]);
        assert!(old.exists() && new.exists());
        assert!(!lock_path(tmp.path()).exists());
        assert!(!tmp
            .path()
            .join("commits")
            .join(crate::commit_lookup::LOCKS_DIR)
            .exists());
    }

    /// Two SHAs of one stripe: while a builder holds the stripe for the
    /// other SHA, gc skips this one, and converges it once the stripe is
    /// free.
    #[test]
    fn test_sweep_stale_generations_same_stripe_other_sha_held_skips_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = two_generations(tmp.path());
        let mut other = sha();
        other[19] ^= 0xff;
        assert_eq!(
            lock_path(tmp.path()),
            sha_lock_path(&tmp.path().join("commits"), &other)
        );
        let held = hold(tmp.path(), &other);
        assert_eq!(
            sweep_stale_generations(tmp.path(), false).unwrap().removed,
            0
        );
        assert!(old.exists() && new.exists());
        drop(held);
        assert_eq!(
            sweep_stale_generations(tmp.path(), false).unwrap().removed,
            1
        );
        assert!(!old.exists() && new.exists());
    }

    /// gc names itself in the owner sidecar, so a builder that waits for the
    /// stripe reports gc and not a builder of its SHA.
    #[test]
    fn test_sweep_sha_lock_real_run_names_gc_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let commits = tmp.path().join("commits");
        let ShaLock::Held(lock) = sweep_sha_lock(&commits, &sha(), false) else {
            panic!("a free stripe is taken");
        };
        assert_eq!(
            FileLock::owner(&lock_path(tmp.path())),
            Some((std::process::id(), format!("gc {SHA}")))
        );
        assert!(matches!(
            sweep_sha_lock(&commits, &sha(), false),
            ShaLock::Busy
        ));
        assert!(matches!(
            sweep_sha_lock(&commits, &sha(), true),
            ShaLock::Busy
        ));
        drop(lock);
        assert!(matches!(
            sweep_sha_lock(&commits, &sha(), true),
            ShaLock::DryRun
        ));
    }

    /// A dry run of every sweep of `commits/` lists what a real run would
    /// retire and writes nothing: no lock dir, no lock file, no sidecar, and
    /// no change to any dir.
    #[test]
    fn test_sweeps_dry_run_commits_tree_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, _new) = two_generations(tmp.path());
        let junk = staging(tmp.path(), 4242);
        let before = tree(tmp.path());
        let generations = sweep_stale_generations(tmp.path(), true).unwrap();
        let staging = sweep_abandoned_staging(tmp.path(), true).unwrap();
        let subtree = sweep_subtree_builds(tmp.path(), true).unwrap();
        let old_format = sweep_old_format_commits(tmp.path(), true).unwrap();
        let unreachable = sweep_unreachable_commits(tmp.path(), true).unwrap();
        assert_eq!(generations.would_remove, vec![old]);
        assert_eq!(staging.would_remove, vec![junk]);
        assert!(subtree.would_remove.is_empty() && old_format.would_remove.is_empty());
        assert!(unreachable.would_remove.is_empty());
        assert_eq!(tree(tmp.path()), before);
    }

    /// An old-style staging dir of the SHA written within the grace, its
    /// lock free: an older ecp may still be building it, so none of the
    /// SHA's generations converge.
    #[test]
    fn test_sweep_stale_generations_recent_legacy_staging_lock_free_skips_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = two_generations(tmp.path());
        let legacy = tmp
            .path()
            .join("commits")
            .join(format!("commit__{SHA}.building"));
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(".build.lock"), b"").unwrap();
        assert_eq!(
            sweep_stale_generations(tmp.path(), false).unwrap().removed,
            0
        );
        assert!(old.exists() && new.exists());
    }

    /// The same leftover, untouched for longer than the grace, no longer
    /// blocks the SHA; gc still leaves the leftover itself in place.
    #[test]
    fn test_sweep_stale_generations_idle_legacy_staging_converges_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = two_generations(tmp.path());
        let legacy = tmp
            .path()
            .join("commits")
            .join(format!("commit__{SHA}.building"));
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("graph.bin"), b"half").unwrap();
        let idle = filetime::FileTime::from_system_time(
            SystemTime::now() - Duration::from_secs(IDLE_GRACE_SECS + 86_400),
        );
        filetime::set_file_mtime(legacy.join("graph.bin"), idle).unwrap();
        filetime::set_file_mtime(&legacy, idle).unwrap();
        assert_eq!(
            sweep_stale_generations(tmp.path(), false).unwrap().removed,
            1
        );
        assert!(!old.exists() && new.exists());
        assert!(legacy.join("graph.bin").exists());
    }

    /// A one-commit repo and its ecp repo root, with one graph of a SHA no
    /// ref names, built and last used 30 days ago.
    fn unreachable_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        use crate::build::orchestrator::test_support::{git, meta};
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q"]);
        fs::write(wt.join("a"), "x").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "x"]);
        let root = tmp.path().join("repo_root");
        let dir = root.join("commits").join(format!("commit__{SHA}"));
        fs::create_dir_all(&dir).unwrap();
        let mut built = meta(SHA, &wt, "v0.0.0+old");
        built.built_at = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        CommitBuildMeta::write_atomic(&dir.join("meta.json"), &built).unwrap();
        fs::write(dir.join("graph.bin"), b"g").unwrap();
        let then = filetime::FileTime::from_system_time(
            SystemTime::now() - Duration::from_secs(30 * 86_400),
        );
        filetime::set_file_atime(dir.join("graph.bin"), then).unwrap();
        filetime::set_file_mtime(&dir, then).unwrap();
        let repo_meta = ecp_core::registry::RepoMeta {
            version: 1,
            common_dir: wt.join(".git").to_string_lossy().into(),
            remote_url: None,
            aliases: vec![],
            known_refs: Default::default(),
            last_built_sha: None,
            total_size_bytes: 0,
            last_touched: chrono::Utc::now().to_rfc3339(),
        };
        ecp_core::registry::RepoMeta::write_atomic(&root.join("meta.json"), &repo_meta).unwrap();
        (tmp, root, dir)
    }

    /// An attach used the graph between gc's first read and its lock: judged
    /// again under the lock, the graph is in use and stays.
    #[test]
    fn test_sweep_unreachable_commits_with_used_before_lock_kept() {
        let (_tmp, root, dir) = unreachable_fixture();
        let reads = Cell::new(0);
        let read = |commits: &Path| {
            reads.set(reads.get() + 1);
            if reads.get() == 2 {
                filetime::set_file_mtime(&dir, filetime::FileTime::now()).unwrap();
            }
            CommitDirListing::read(commits)
        };
        let fs_ops = CommitsFs {
            read_listing: &read,
            sync_dir: &ecp_core::registry::sync_dir,
        };
        let stats = sweep_unreachable_commits_with(&root, false, &fs_ops).unwrap();
        assert_eq!(reads.get(), 2, "the listing is read again under the lock");
        assert_eq!((stats.marked, stats.removed), (0, 0));
        assert!(dir.exists());
    }

    /// The graph is retired only after a successful fsync of `commits/`.
    #[test]
    fn test_sweep_unreachable_commits_with_fsync_fails_kept_then_retired() {
        let (_tmp, root, dir) = unreachable_fixture();
        let fail = |_: &Path| -> io::Result<()> { Err(io::Error::other("injected")) };
        let failing = CommitsFs {
            read_listing: &CommitDirListing::read,
            sync_dir: &fail,
        };
        assert_eq!(
            sweep_unreachable_commits_with(&root, false, &failing)
                .unwrap()
                .removed,
            0
        );
        assert!(dir.exists());
        let present_at_sync = Cell::new(None);
        let record = |_: &Path| -> io::Result<()> {
            present_at_sync.set(Some(dir.exists()));
            Ok(())
        };
        let recording = CommitsFs {
            read_listing: &CommitDirListing::read,
            sync_dir: &record,
        };
        assert_eq!(
            sweep_unreachable_commits_with(&root, false, &recording)
                .unwrap()
                .removed,
            1
        );
        assert_eq!(present_at_sync.get(), Some(true));
        assert!(!dir.exists());
    }

    /// Subtree and old-format sweeps retire a SHA's dir only under its
    /// stripe lock.
    #[test]
    fn test_sweep_subtree_builds_sha_lock_held_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("repo");
        let subtree = worktree.join("sub");
        fs::create_dir_all(&subtree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        let dir = tmp.path().join("commits").join(format!("commit__{SHA}"));
        fs::create_dir_all(&dir).unwrap();
        CommitBuildMeta::write_atomic(
            &dir.join("meta.json"),
            &crate::build::orchestrator::test_support::meta(SHA, &subtree, "v0.0.0+old"),
        )
        .unwrap();
        let held = hold(tmp.path(), &sha());
        assert_eq!(sweep_subtree_builds(tmp.path(), false).unwrap().removed, 0);
        assert!(dir.exists());
        drop(held);
        assert_eq!(sweep_subtree_builds(tmp.path(), false).unwrap().removed, 1);
        assert!(!dir.exists());
    }

    fn staging(root: &Path, pid: u32) -> PathBuf {
        settled_dir(
            &root.join("commits"),
            &staging_dir_name(&format!("commit__{SHA}"), pid, 0),
        )
    }

    /// F5: a crash before the first publish leaves staging with no published
    /// dir beside it; the generation sweep never sees that SHA.
    #[test]
    fn test_sweep_abandoned_staging_no_published_dir_retired() {
        let tmp = tempfile::tempdir().unwrap();
        let junk = staging(tmp.path(), 4242);
        let stats = sweep_abandoned_staging(tmp.path(), false).unwrap();
        assert_eq!((stats.marked, stats.removed), (1, 1));
        assert!(!junk.exists());
    }

    /// F5: one published dir is no group of two, so the generation sweep
    /// skips the SHA; the staging sweep still retires the staging.
    #[test]
    fn test_sweep_abandoned_staging_one_published_dir_staging_retired_published_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let published = settled_dir(&tmp.path().join("commits"), &format!("commit__{SHA}"));
        let junk = staging(tmp.path(), 4242);
        let stats = sweep_abandoned_staging(tmp.path(), false).unwrap();
        assert_eq!(stats.removed, 1);
        assert!(!junk.exists() && published.exists());
    }

    #[test]
    fn test_sweep_abandoned_staging_sha_lock_held_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let live = staging(tmp.path(), std::process::id());
        let held = hold(tmp.path(), &sha());
        let stats = sweep_abandoned_staging(tmp.path(), false).unwrap();
        assert_eq!((stats.marked, stats.removed), (0, 0));
        assert!(live.exists());
        drop(held);
    }

    /// F1: an older ecp may own `<base>.building/` even with its lock free
    /// (it releases just before its rename); doctor reports it, gc keeps it.
    #[test]
    fn test_sweep_abandoned_staging_legacy_staging_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = settled_dir(
            &tmp.path().join("commits"),
            &format!("commit__{SHA}.building"),
        );
        let stats = sweep_abandoned_staging(tmp.path(), false).unwrap();
        assert_eq!(stats.marked, 0);
        assert!(legacy.join("graph.bin").exists());
    }

    #[test]
    fn test_sweep_abandoned_staging_dry_run_listed_kept_no_lock_file() {
        let tmp = tempfile::tempdir().unwrap();
        let junk = staging(tmp.path(), 4242);
        let stats = sweep_abandoned_staging(tmp.path(), true).unwrap();
        assert_eq!(stats.would_remove, vec![junk.clone()]);
        assert!(junk.exists());
        assert!(!lock_path(tmp.path()).exists());
    }

    /// F5: an attach never enters the build path, so the staging a crashed
    /// builder left stays until gc's staging sweep retires it.
    #[test]
    fn test_sweep_abandoned_staging_after_attach_retired() {
        use crate::build::orchestrator::test_support::*;
        let fx = fx();
        let built = build_at(&fx, &fx.repo_root);
        let junk = fx.commits.join(staging_dir_name(&fx.base, 999_999, 0));
        fs::create_dir_all(&junk).unwrap();
        let attached = build_at(&fx, &fx.repo_root);
        assert_eq!(
            attached.commit_dir, built.commit_dir,
            "the second build attaches"
        );
        assert!(junk.exists());
        let stats = sweep_abandoned_staging(&fx.repo_root, false).unwrap();
        assert_eq!(stats.removed, 1);
        assert!(!junk.exists());
        assert!(attached.commit_dir.exists());
    }
}
