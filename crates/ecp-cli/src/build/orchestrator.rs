//! L2 build orchestrator: src resolution → analyzer pipeline → atomic publish.
//!
//! One lock per repo and SHA stripe, `commits/.locks/<xx>.lock`
//! ([`crate::commit_lookup::sha_lock_path`]), held from the last check for an
//! existing build through the publish rename and the fsync of `commits/`.
//! Under it the builder picks a name above every existing build of the SHA
//! and builds in a staging dir of its own; a second builder of the SHA waits
//! and then attaches to what the first published.

use crate::build::dirname_picker::pick_dirname;
use crate::commit_lookup::{
    record_sha_lock_owner, sha_lock_path, staging_dir_name, CommitDirListing, CommitIndex,
    StagingShape, LOCKS_DIR,
};
use crate::git::safe_exec;
use crate::repo_identity::repo_dir_name_for_cwd;
use ecp_core::registry::{
    resolve_home_ecp, CommitBuildMeta, EmbeddingStatus, FileLock, Generation, RefRecord,
    RegistryFile, RepoAlias, RepoMeta, SourceType, BUILDER_FINGERPRINT,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static GENERATION_COUNTER: AtomicU64 = AtomicU64::new(0);
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The one wait budget for every builder of a SHA another builder holds.
/// A holder can spend up to 120 s waiting for a machine-wide build slot
/// (`semaphore::acquire`) before it builds, so the 5 s of the old completion
/// poll timed out on a healthy holder. The largest corpus measured here
/// (14882 files) cold-indexes in 5.3 s median and 7.5 s worst. A wedged
/// holder surfaces as an error naming it, never as a hang.
pub(crate) const BUILD_LOCK_TIMEOUT: Duration = Duration::from_secs(300);

pub struct BuildResult {
    pub commit_dir: PathBuf,
    pub sha_hex: String,
    pub source_type: SourceType,
    /// Background tantivy writer (CI-B). `None` for fast-path attaches or
    /// when no L2 was built fresh. CLI `admin index` calls
    /// [`BuildResult::join_background`] before returning to the shell so
    /// the subprocess does not exit while tantivy is still writing temp
    /// segments under the publish dir — that race was the source of the
    /// `tantivy/.tmpXXX: No such file or directory` test failures on
    /// Linux + macOS (CI-M). Long-lived callers (MCP, `auto_ensure`) can
    /// drop the handle to keep the fire-and-forget behaviour.
    pub tantivy_handle: Option<std::thread::JoinHandle<()>>,
}

impl BuildResult {
    /// Join the deferred tantivy writer if one was spawned. Idempotent
    /// (handle is taken out of the result). Errors from the background
    /// thread are logged at `warn`; tantivy failures do not bubble up
    /// because `ecp find` already falls back to substring scan when the
    /// index is missing.
    pub fn join_background(&mut self) {
        if let Some(h) = self.tantivy_handle.take() {
            if let Err(panic) = h.join() {
                tracing::warn!("tantivy background thread panicked: {panic:?}");
            }
        }
    }
}

/// The SHA as bytes. Every name and path a build derives from it is the
/// lowercase hex of these bytes, so `ABC…` and `abc…` share one lock and one
/// set of dirs.
pub(crate) fn parse_sha(text: &str) -> io::Result<[u8; 20]> {
    let mut sha = [0u8; 20];
    if text.len() != 40 || hex::decode_to_slice(text, &mut sha).is_err() {
        return Err(io::Error::other(format!("invalid sha: {text}")));
    }
    Ok(sha)
}

pub fn build_l2(worktree: &Path, target_sha: Option<&str>) -> io::Result<BuildResult> {
    // The commit slot is keyed by sha alone. A build started from a
    // subdirectory would publish that subtree as the commit's graph, and every
    // later query for the sha, from any directory, would attach to it and see
    // nothing else. Resolve to the worktree root before anything is keyed.
    let toplevel = worktree_toplevel(worktree);
    let worktree = toplevel.as_deref().unwrap_or(worktree);
    let sha = match target_sha {
        Some(s) => parse_sha(s)?,
        None => parse_sha(&head_sha_hex(worktree)?)?,
    };
    let repo_root = resolve_home_ecp().join(repo_dir_name_for_cwd(worktree)?);
    build_l2_at(worktree, &sha, &repo_root, report_wait)
}

/// [`build_l2`] for a resolved worktree root, SHA and repo root. `on_wait`
/// runs once if another builder holds the SHA.
pub(crate) fn build_l2_at(
    worktree: &Path,
    sha: &[u8; 20],
    repo_root: &Path,
    on_wait: impl FnOnce(&Holder),
) -> io::Result<BuildResult> {
    build_l2_recorded_at(worktree, worktree, sha, repo_root, on_wait)
}

/// [`build_l2`] of `sha` from `checkout`, a checkout of that commit that the
/// caller deletes afterwards (`ecp diff`'s baseline worktree). The build
/// records `recorded_root`, the user's repository root, as the worktree it
/// was built from. Recording the checkout made every later lookup reject the
/// slot as built below a root that no longer exists, so each diff indexed
/// its baseline again and left one more generation behind.
pub(crate) fn build_l2_recorded(
    checkout: &Path,
    recorded_root: &Path,
    sha_hex: &str,
) -> io::Result<BuildResult> {
    let sha = parse_sha(sha_hex)?;
    let repo_root = resolve_home_ecp().join(repo_dir_name_for_cwd(checkout)?);
    build_l2_recorded_at(checkout, recorded_root, &sha, &repo_root, report_wait)
}

/// [`build_l2_at`] that records `recorded_root` as the worktree the build
/// came from; see [`build_l2_recorded`].
pub(crate) fn build_l2_recorded_at(
    worktree: &Path,
    recorded_root: &Path,
    sha: &[u8; 20],
    repo_root: &Path,
    on_wait: impl FnOnce(&Holder),
) -> io::Result<BuildResult> {
    let sha_hex = hex::encode(sha);
    let commits_dir = repo_root.join("commits");
    fs::create_dir_all(&commits_dir)?;

    // Fast path: same SHA already built by a binary with a matching
    // fingerprint → reuse without touching the analyzer pipeline.
    // L2 is SHA-pure (v2 layout, PR #55); working-tree drift goes through
    // the L1 session overlay, not here.
    if let Some(attached) = attach_latest_if_fingerprint_matches(&commits_dir, &sha_hex, worktree) {
        return Ok(attached);
    }
    let base = pick_dirname(worktree, &sha_hex)?;
    let sha_lock = acquire_sha_lock(&commits_dir, sha, BUILD_LOCK_TIMEOUT, on_wait)?;
    // Checked again under the lock: a builder that held it may have
    // published exactly this build while this one waited.
    if let Some(attached) = attach_latest_if_fingerprint_matches(&commits_dir, &sha_hex, worktree) {
        return Ok(attached);
    }
    build_inside_locked(
        worktree,
        recorded_root,
        sha,
        repo_root,
        &base,
        sha_lock,
        &DurablePublish,
    )
}

/// Who holds a SHA lock, read when a builder starts to wait for it and again
/// when the wait ends in a timeout.
pub(crate) struct Holder {
    sha8: String,
    pid: Option<u32>,
    role: Option<String>,
    phase: HolderPhase,
}

enum HolderPhase {
    /// The owner sidecar names no SHA: a holder between taking the lock and
    /// naming itself, or an ecp that writes its PID only.
    Unknown,
    /// The stripe is held for another SHA with the same first byte. That
    /// holder is not building this SHA; this build waits only for the stripe.
    OtherSha(String),
    Gc,
    /// A builder of this SHA that has no staging dir yet: it waits for a
    /// build slot.
    SlotWait,
    /// A builder of this SHA, in its own staging dir for `age` seconds.
    Building {
        staging: String,
        age: u64,
    },
}

impl Holder {
    fn read(commits_dir: &Path, sha: &[u8; 20], lock_path: &Path) -> Self {
        let owner = FileLock::owner(lock_path);
        let pid = owner.as_ref().map(|(pid, _)| *pid);
        let mut words = owner
            .as_ref()
            .map_or("", |(_, rest)| rest)
            .split_whitespace();
        let role = words.next().map(str::to_string);
        let phase = match words.next().and_then(|key| parse_sha(key).ok()) {
            None => HolderPhase::Unknown,
            Some(key) if key != *sha => HolderPhase::OtherSha(hex::encode(key)[..8].to_string()),
            Some(_) if role.as_deref() == Some("gc") => HolderPhase::Gc,
            Some(_) => pid
                .and_then(|pid| own_staging(commits_dir, sha, pid))
                .map_or(HolderPhase::SlotWait, |(staging, age)| {
                    HolderPhase::Building { staging, age }
                }),
        };
        Self {
            sha8: hex::encode(sha)[..8].to_string(),
            pid,
            role,
            phase,
        }
    }
}

/// The staging dir builder `pid` keeps for `sha`, and its age in seconds.
fn own_staging(commits_dir: &Path, sha: &[u8; 20], pid: u32) -> Option<(String, u64)> {
    let listing = CommitDirListing::read(commits_dir).ok()?;
    let dir = listing.staging.into_iter().find(|s| {
        s.sha == *sha && matches!(s.shape, StagingShape::PerAttempt { pid: p, .. } if p == pid)
    })?;
    let age = fs::metadata(commits_dir.join(&dir.name))
        .and_then(|m| m.created().or_else(|_| m.modified()))
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map_or(0, |d| d.as_secs());
    Some((dir.name, age))
}

/// `key=value` fields, so an agent reading stderr can split them. A holder
/// of the stripe for another SHA is `stripe_holder`, never `holder`: it is
/// not this SHA's builder.
impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = match self.phase {
            HolderPhase::OtherSha(_) => "stripe_holder",
            _ => "holder",
        };
        write!(f, "sha={} {key}=", self.sha8)?;
        match self.pid {
            Some(pid) => write!(f, "{pid}")?,
            None => f.write_str("?")?,
        }
        write!(f, " role={}", self.role.as_deref().unwrap_or("?"))?;
        match &self.phase {
            HolderPhase::Unknown => f.write_str(" phase=unknown"),
            HolderPhase::OtherSha(other) => write!(f, " phase=other-sha held_for={other}"),
            HolderPhase::Gc => f.write_str(" phase=gc"),
            HolderPhase::SlotWait => f.write_str(" phase=slot-wait"),
            HolderPhase::Building { staging, age } => {
                write!(f, " phase=building staging={staging} age={age}s")
            }
        }
    }
}

/// The production `on_wait`: one stderr line when a build starts to wait.
pub(crate) fn report_wait(holder: &Holder) {
    eprintln!("l2.wait {holder}");
}

/// Take the SHA's stripe lock and name this process its builder. When it is
/// busy: call `on_wait` once with the holder, and wait up to `budget`.
/// Fairness is not promised (`flock` / `LockFileEx` give none), only the
/// bound. The caller checks the publish state of its SHA again once it holds
/// the lock.
pub(crate) fn acquire_sha_lock(
    commits_dir: &Path,
    sha: &[u8; 20],
    budget: Duration,
    on_wait: impl FnOnce(&Holder),
) -> io::Result<FileLock> {
    let lock_path = sha_lock_path(commits_dir, sha);
    fs::create_dir_all(commits_dir.join(LOCKS_DIR))?;
    match FileLock::try_exclusive(&lock_path) {
        Ok(lock) => {
            record_sha_lock_owner(&lock_path, "build", sha);
            return Ok(lock);
        }
        Err(e) if !ecp_core::registry::is_lock_contended(&e) => return Err(e),
        Err(_) => {}
    }
    on_wait(&Holder::read(commits_dir, sha, &lock_path));
    match FileLock::acquire_exclusive_within(&lock_path, budget) {
        Ok(lock) => {
            record_sha_lock_owner(&lock_path, "build", sha);
            Ok(lock)
        }
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "l2.wait timeout {} waited={}s",
                Holder::read(commits_dir, sha, &lock_path),
                budget.as_secs()
            ),
        )),
        Err(e) => Err(e),
    }
}

/// The filesystem steps of a publish, so a test can fail one or record their
/// order.
pub(crate) trait Publish {
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
}

pub(crate) struct DurablePublish;

impl Publish for DurablePublish {
    fn sync_file(&self, path: &Path) -> io::Result<()> {
        sync_file(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        ecp_core::registry::sync_dir(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        ecp_core::registry::rename_with_retry(from, to)
    }
}

/// The name this build publishes under: bare `base` when the SHA has no dir,
/// else `<base>.gen.<ts>.<pid>.<counter>` with `ts` strictly above every
/// same-SHA timestamp, whatever the clock says. `Err` when that would
/// overflow (`u64::MAX` already used), which refuses the publish.
pub(crate) fn next_publish_name(
    base: &str,
    same_sha: &[Option<Generation>],
    now_ms: u64,
    pid: u32,
    counter: u32,
) -> io::Result<String> {
    if same_sha.is_empty() {
        return Ok(base.to_string());
    }
    let newest = same_sha
        .iter()
        .flatten()
        .map(|g| g.timestamp_ms)
        .max()
        .unwrap_or(0);
    let floor = newest
        .checked_add(1)
        .ok_or_else(|| io::Error::other(format!("generation timestamps exhausted for {base}")))?;
    let generation = Generation {
        timestamp_ms: now_ms.max(floor),
        pid,
        counter,
    };
    Ok(format!("{base}{}", generation.format_suffix()))
}

/// [`next_publish_name`] over a listing, which must be complete: a missed
/// entry could hide the newest generation and let this build rank below it.
pub(crate) fn choose_publish_name(
    listing: &CommitDirListing,
    sha: &[u8; 20],
    base: &str,
    now_ms: u64,
) -> io::Result<String> {
    if !listing.complete {
        return Err(io::Error::other(format!(
            "commits listing incomplete; refusing to name a build of {base}"
        )));
    }
    let same_sha: Vec<Option<Generation>> =
        listing.same_sha(sha).map(|d| d.parsed.generation).collect();
    // Wrap-around at u32::MAX of the process-local counter is safe: the
    // timestamp already rises above every existing generation.
    next_publish_name(
        base,
        &same_sha,
        now_ms,
        std::process::id(),
        GENERATION_COUNTER.fetch_add(1, Ordering::Relaxed) as u32,
    )
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// Retire the per-attempt staging dirs of `sha`. Only a holder of the SHA lock
/// calls this, and a builder keeps a staging dir only while it holds that
/// lock, so every one found is abandoned. The PID in the name proves nothing;
/// PIDs are reused.
pub(crate) fn retire_abandoned_staging(
    listing: &CommitDirListing,
    sha: &[u8; 20],
    commits_dir: &Path,
) {
    for staging in listing
        .staging
        .iter()
        .filter(|s| s.sha == *sha && matches!(s.shape, StagingShape::PerAttempt { .. }))
    {
        if let Err(e) = ecp_core::registry::retire_dir_async(&commits_dir.join(&staging.name)) {
            eprintln!("l2: retire abandoned staging {}: {e}", staging.name);
        }
    }
}

/// A staging dir this attempt owns: `create_dir` fails when the name exists,
/// so success proves no other attempt uses it.
fn create_staging(commits_dir: &Path, base: &str) -> io::Result<PathBuf> {
    let pid = std::process::id();
    let mut last = None;
    for _ in 0..64 {
        let attempt = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = commits_dir.join(staging_dir_name(base, pid, attempt));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            // A dir a crashed process with this PID left behind.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no free staging name")))
}

/// Build the SHA under the held `sha_lock` and publish it. Shared between
/// `build_l2` (first build / SHA drift) and `force_rebuild_l2`.
/// `recorded_root` is the worktree `meta.json` names as the build's source:
/// `worktree` itself, except for a checkout the caller deletes afterwards.
///
/// Order, all under the SHA lock: complete listing → choose the name → retire
/// abandoned staging → build slot → own staging dir → analyzer, `meta.json`
/// and the build-time sidecars into staging → fsync files and dirs bottom-up
/// → rename → fsync `commits/` → drop this process's scan cache →
/// `.head_sha`. Then the slot and the lock go, and `_src` retirement,
/// tantivy and the repo meta follow. gc retires a previous generation only
/// under the same lock, so never before the `commits/` fsync.
///
/// Once the rename happened every step after it runs, even when the fsync of
/// `commits/` failed: the dir is published, and readers use it. That fsync
/// error returns last.
///
/// The lock file sits in `commits/.locks/`, outside the renamed dir, so
/// Windows can rename while it stays held.
pub(crate) fn build_inside_locked(
    worktree: &Path,
    recorded_root: &Path,
    sha: &[u8; 20],
    repo_root: &Path,
    base: &str,
    sha_lock: FileLock,
    publish: &dyn Publish,
) -> io::Result<BuildResult> {
    let sha_hex = hex::encode(sha);
    let commits_dir = repo_root.join("commits");
    let listing = CommitDirListing::read(&commits_dir)?;
    let name = choose_publish_name(&listing, sha, base, now_ms())?;
    retire_abandoned_staging(&listing, sha, &commits_dir);

    // Machine-wide build throttle. The SHA lock already prevents duplicate
    // work for one SHA; this additionally bounds how many *different* repos
    // rebuild at once, since each build issues ~180 MB of I/O and concurrent
    // builds saturate the WSL2 vhdx and hang the host. Degrades open
    // (unthrottled) if the slot machinery errors. `repo_root`'s parent is the
    // ecp home the caller resolved.
    let build_slot = crate::build::semaphore::acquire(repo_root.parent().unwrap_or(repo_root));
    let staging = create_staging(&commits_dir, base)?;

    let prof = std::env::var("ECP_PROF").is_ok();
    let published = build_into_staging(worktree, recorded_root, &sha_hex, repo_root, &staging)
        .and_then(|(graph, source_type)| {
            let t_publish = std::time::Instant::now();
            let dir = publish_staging(&staging, &commits_dir, &name, publish)?;
            if prof {
                eprintln!(
                    "prof orchestrator.publish: {:.3}s",
                    t_publish.elapsed().as_secs_f32()
                );
            }
            Ok((graph, source_type, dir))
        });
    let (
        global_graph,
        source_type,
        Published {
            dir: publish_dir,
            sync_error,
        },
    ) = match published {
        Ok(published) => published,
        Err(e) => {
            // Still under the SHA lock, so this attempt's dir is retired
            // before anyone else may judge it abandoned. After a successful
            // rename the staging path is gone and this is a no-op.
            let _ = ecp_core::registry::retire_dir_async(&staging);
            drop(build_slot);
            drop(sha_lock);
            return Err(e);
        }
    };
    // Write the HEAD-SHA fingerprint next to the freshly published graph.bin
    // so subsequent read commands can short-circuit `auto_ensure::ensure_index`
    // without walking the working tree.
    crate::auto_ensure::write_head_sha_sidecar_with_sha(&publish_dir.join("graph.bin"), &sha_hex);
    drop(build_slot);
    drop(sha_lock);

    let _ = ecp_core::registry::retire_dir_async(&publish_dir.join("_src"));

    // CI-B: tantivy index build, deferred to background thread NOW that
    // publish_dir is the final location. `ecp find` falls back to
    // substring scan when tantivy is missing (see find.rs), so a slow
    // bm25 query immediately after build is the worst-case degradation.
    // global_graph moves into the thread.
    //
    // CI-M: handle is returned in `BuildResult.tantivy_handle` so the
    // CLI `admin index` path joins before exit. Without joining, a
    // subprocess invocation (e.g. a test driving `ecp admin index` then
    // dropping its TempDir) would race with the still-writing tantivy
    // thread → `tantivy/.tmpXXX: No such file or directory`. Long-lived
    // callers (MCP, auto_ensure) can drop the handle to keep
    // fire-and-forget semantics.
    let tantivy_dir = publish_dir.clone();
    let tantivy_handle = std::thread::spawn(move || {
        if let Err(e) = crate::search::TantivyEngine::build_index(&tantivy_dir, &global_graph) {
            tracing::warn!(
                "Full-text index build failed for {:?}: {}; exact-name queries still work",
                tantivy_dir,
                e
            );
        }
    });

    let t_repo_meta = std::time::Instant::now();
    let repo_meta = update_repo_meta(repo_root, worktree, &sha_hex);
    if prof {
        eprintln!(
            "prof orchestrator.update_repo_meta: {:.3}s",
            t_repo_meta.elapsed().as_secs_f32()
        );
    }
    if let Some(e) = sync_error {
        // A caller that gets an error may exit at once, and a tantivy writer
        // cut off mid-index leaves its temp dir in the published graph.
        if let Err(panic) = tantivy_handle.join() {
            tracing::warn!("tantivy background thread panicked: {panic:?}");
        }
        return Err(e);
    }
    repo_meta?;

    Ok(BuildResult {
        commit_dir: publish_dir,
        sha_hex,
        source_type,
        tantivy_handle: Some(tantivy_handle),
    })
}

/// Analyzer pipeline, `meta.json` and the build-time sidecars, all into
/// `staging`. Returns the graph for the tantivy writer and the source type.
fn build_into_staging(
    worktree: &Path,
    recorded_root: &Path,
    sha_hex: &str,
    repo_root: &Path,
    staging: &Path,
) -> io::Result<(ecp_core::graph::ZeroCopyGraph, SourceType)> {
    let prof = std::env::var("ECP_PROF").is_ok();
    let t_src_root = std::time::Instant::now();
    let src_root = if worktree_clean_and_head_matches(worktree, sha_hex)? {
        worktree.to_path_buf()
    } else {
        let src = staging.join("_src");
        fs::create_dir(&src)?;
        git_archive_to(worktree, sha_hex, &src)?;
        src
    };
    if prof {
        eprintln!(
            "prof orchestrator.src_root: {:.3}s",
            t_src_root.elapsed().as_secs_f32()
        );
    }

    // Analyzer pipeline. `repo_root` doubles as the persistent parse_cache
    // root — cache entries live in `<repo_root>/parse_cache/<fp>/` and
    // survive across L2 commit_dirs as long as the file content (and binary
    // build) is unchanged.
    //
    // CI-B: receives `(node_count, global_graph)`; tantivy is built on a
    // background thread AFTER the rename so the writer targets the final
    // publish dir, not the staging dir.
    let (node_count, global_graph) = crate::commands::admin::index::run_analyzer_for_paths(
        &src_root,
        staging,
        Some(repo_root),
        None,
    )?;

    let t_refs = std::time::Instant::now();
    let refs_at_build = collect_refs(worktree, sha_hex)?;
    let source_type = source_type_from_refs(&refs_at_build);
    let source_id = source_id_from_refs(&refs_at_build);
    let parent = parent_sha(worktree, sha_hex).ok();
    if prof {
        eprintln!(
            "prof orchestrator.refs+parent: {:.3}s",
            t_refs.elapsed().as_secs_f32()
        );
    }

    let fingerprint = BUILDER_FINGERPRINT;
    let meta = CommitBuildMeta {
        version: 1,
        sha: sha_hex.to_string(),
        source_type,
        source_id,
        built_from_worktree: recorded_root.to_string_lossy().into(),
        built_at: chrono::Utc::now().to_rfc3339(),
        parent_sha: parent,
        node_count: node_count as u32,
        embedding_status: EmbeddingStatus::None,
        refs_at_build,
        refs_seen_since: vec![],
        builder_fingerprint: Some(fingerprint.to_string()),
        binary_commit_sha: Some(env!("ECP_GIT_SHA").to_string()),
    };
    CommitBuildMeta::write_atomic(&staging.join("meta.json"), &meta)?;
    // Inside staging, so the rename publishes them with graph.bin: a reader
    // never sees this graph without them, and no process exit can lose them.
    // The fingerprint sidecar lets `ensure_index` detect convention drift
    // (FU-2026-05-25-005); the version sidecar spares warm-attach the
    // `rkyv::access` validation.
    let graph_bin = staging.join("graph.bin");
    crate::auto_ensure::write_builder_fingerprint_sidecar(&graph_bin, fingerprint)?;
    crate::auto_ensure::write_compatible_version_sidecar(&graph_bin)?;
    Ok((global_graph, source_type))
}

/// A publish whose rename happened.
pub(crate) struct Published {
    pub dir: PathBuf,
    /// The fsync of `commits/` after the rename failed. The dir is published
    /// all the same (gc keeps the previous generation until its own fsync of
    /// `commits/` succeeds), so the caller finishes the publish and returns
    /// this error last.
    pub sync_error: Option<io::Error>,
}

/// fsync every file and dir in `staging` bottom-up → rename to `name` →
/// fsync `commits_dir` → drop this process's scan cache. The rename is the
/// publication: `Err` means nothing was published.
pub(crate) fn publish_staging(
    staging: &Path,
    commits_dir: &Path,
    name: &str,
    publish: &dyn Publish,
) -> io::Result<Published> {
    sync_tree(staging, publish)?;
    let dir = commits_dir.join(name);
    publish.rename(staging, &dir)?;
    let sync_error = publish.sync_dir(commits_dir).err().map(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "published {} but the fsync of {} failed: {e}",
                dir.display(),
                commits_dir.display()
            ),
        )
    });
    CommitIndex::invalidate(commits_dir);
    Ok(Published { dir, sync_error })
}

pub(crate) fn attach_latest_if_fingerprint_matches(
    commits_dir: &Path,
    sha_hex: &str,
    worktree: &Path,
) -> Option<BuildResult> {
    let sha = parse_sha(sha_hex).ok()?;
    let idx = CommitIndex::scan(commits_dir).ok()?;
    let dir = idx.find(&sha)?;
    attach_if_fingerprint_matches(&commits_dir.join(dir), worktree)
}

/// A slot published from below the worktree root holds only that subtree's
/// graph under the commit's sha, so `find` answers `found:false` for the rest
/// of the repository. `build_l2` now resolves the root before it keys the
/// slot; slots written earlier are healed by rejecting them wherever a slot is
/// chosen. The recorded worktree is absolute, is not the root in use, and has
/// no `.git` entry, so it was never a git worktree root. A non-git source tree
/// has no `.git` either, but its slot is keyed by the path-bound sha that
/// `head_sha_hex` synthesises, which exempts it. A recorded git root that no
/// longer exists is rejected on the same rule; one rebuild republishes the
/// slot from the current root.
pub(crate) fn built_below_worktree_root(built: &Path, sha: &str, worktree: &Path) -> bool {
    built.is_absolute()
        && built != worktree
        && !built.join(".git").exists()
        && !path_bound_sha(built).is_ok_and(|own| own.eq_ignore_ascii_case(sha))
}

/// Cheap pre-build check: if `commit_dir/meta.json` exists and its
/// `builder_fingerprint` matches the current binary, the published L2 at
/// this SHA was made by an equivalent build — return it instead of
/// rebuilding. Shared between `build_l2` (skip-if-exists fast path and the
/// re-check under the SHA lock) and `admin index` (reuse only what
/// `build_l2` would reuse).
pub(crate) fn attach_if_fingerprint_matches(
    commit_dir: &Path,
    worktree: &Path,
) -> Option<BuildResult> {
    if !commit_dir.join("meta.json").is_file() {
        return None;
    }
    let meta = CommitBuildMeta::read(&commit_dir.join("meta.json")).ok()?;
    // A matching meta.json says nothing about graph.bin itself: a torn or
    // swapped graph.bin behind a current meta would be handed back on every
    // rebuild request, and the query that asked for the rebuild then fails to
    // load it. The validation result is cached, so the load pays nothing extra.
    if meta.builder_fingerprint.as_deref() != Some(BUILDER_FINGERPRINT)
        || built_below_worktree_root(Path::new(&meta.built_from_worktree), &meta.sha, worktree)
        || !crate::engine::header_compatible(&commit_dir.join("graph.bin"))
    {
        return None;
    }
    let graph_bin = commit_dir.join("graph.bin");
    // Back-fill the HEAD-SHA sidecar for graphs published by binaries that
    // pre-date the auto_ensure shortcut. One-shot until the next rebuild.
    crate::auto_ensure::write_head_sha_sidecar_with_sha(&graph_bin, &meta.sha);
    // Back-fill the fingerprint sidecar from the build's own meta.json, so
    // `ensure_index` can skip the meta.json read on later queries. A lost
    // write only costs that read.
    if let Some(fingerprint) = meta.builder_fingerprint.as_deref() {
        let _ = crate::auto_ensure::write_builder_fingerprint_sidecar(&graph_bin, fingerprint);
    }
    Some(BuildResult {
        commit_dir: commit_dir.to_path_buf(),
        sha_hex: meta.sha,
        source_type: meta.source_type,
        tantivy_handle: None,
    })
}

/// `None` outside a git worktree; callers then keep the directory they were given.
pub(crate) fn worktree_toplevel(dir: &Path) -> Option<PathBuf> {
    let out = safe_exec::git()
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    // Only git's terminator goes; a root ending in whitespace is still that root.
    let path = path.strip_suffix('\n').unwrap_or(&path);
    let path = path.strip_suffix('\r').unwrap_or(path);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub(crate) fn head_sha_hex(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree)
        .output()?;
    if out.status.success() {
        return Ok(std::str::from_utf8(&out.stdout)
            .map_err(io::Error::other)?
            .trim()
            .to_string());
    }
    // Non-git fallback: synthesize a stable 40-hex SHA-shaped digest from the
    // canonical worktree path. The cache path layout stays uniform
    // (`branch_main__<hex40>.gen.X/`) so downstream code is unchanged; staleness
    // detection still flows through the mtime walk in `auto_ensure`.
    // Identity is path-bound — moving the dir invalidates this digest, treated
    // as a new repo (acceptable for ad-hoc indexing of non-VCS source trees).
    path_bound_sha(worktree)
}

/// The sha a non-git source tree is keyed by: a digest of its canonical path.
pub(crate) fn path_bound_sha(worktree: &Path) -> io::Result<String> {
    let canonical = std::fs::canonicalize(worktree)?;
    let h = xxhash_rust::xxh3::xxh3_128(canonical.to_string_lossy().as_bytes());
    Ok(format!("{h:040x}"))
}

pub(crate) fn worktree_clean_and_head_matches(worktree: &Path, sha: &str) -> io::Result<bool> {
    if head_sha_hex(worktree)? != sha {
        return Ok(false);
    }
    // Non-git: the synthetic digest from head_sha_hex is path-bound — equality
    // above already establishes "we're at the expected snapshot". No dirty-tree
    // concept applies; treat as clean and index the worktree directly.
    if crate::git_cache::common_dir(worktree).is_err() {
        return Ok(true);
    }
    // `git status`, not `git diff-index --quiet HEAD`. diff-index does not see
    // untracked files, so a tree whose only change was a new file read as clean
    // and got indexed from the worktree — under HEAD's SHA, into a
    // `commits/<branch>__<sha>/` slot every worktree of this repo shares. The
    // next worktree at that SHA then found a symbol that is not in HEAD.
    //
    // This is the same question `auto_ensure`'s freshness gate asks, and it now
    // gets the same answer from the same code: same flags, same artifact
    // filter. Two definitions of "clean" that disagree is what let this
    // through. The plain command rather than the private index: a wrong
    // "clean" here poisons that shared slot, and a build costs seconds anyway.
    let out = crate::git::status::plain(worktree)?;
    if !out.status.success() {
        return Ok(false);
    }
    let home_ecp_canonical = fs::canonicalize(resolve_home_ecp()).ok();
    let dirty = crate::auto_ensure::parse_porcelain_paths(&out.stdout, worktree)
        .into_iter()
        .any(|p| !crate::auto_ensure::is_ecp_artifact(&p, None, home_ecp_canonical.as_deref()));
    Ok(!dirty)
}

/// Extract `sha`'s tree into `dest`.
///
/// `archive` converts content on the way out, so it runs any filter driver the
/// repository names for a path. This is the widest reach of the three such
/// commands: `build_l2` comes through here whenever the worktree is dirty or
/// HEAD is not the target SHA, which nearly every command can trigger. The
/// overrides are what stop the scanned repository executing its own program.
pub(crate) fn git_archive_to(worktree: &Path, sha: &str, dest: &Path) -> io::Result<()> {
    use std::io::Read;

    let overrides = safe_exec::filter_overrides(worktree);
    // git's stdout is tar's stdin, so the kernel pipe holds the archive rather
    // than this process. `.output()` used to buffer the whole tar first, which
    // made peak memory a function of what the repository tracks: a 200 MB blob
    // took the index from 71 MB to 502 MB, and none of it is content the
    // analyzer would read — its per-file cap rejects that file afterwards.
    let mut git = safe_exec::git_with_overrides(worktree, &overrides)
        .args(["archive", "--format=tar", sha])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    // Every path out of here from now on reaps git. Dropping a `Child` neither
    // kills nor waits for the process, so an early `?` would leave `git
    // archive` running against the repository with nobody collecting it.
    let finish_git = |git: &mut std::process::Child| {
        let _ = git.kill();
        let _ = git.wait();
    };

    let Some(git_stdout) = git.stdout.take() else {
        finish_git(&mut git);
        return Err(io::Error::other("git archive: no stdout"));
    };
    let Some(mut git_stderr) = git.stderr.take() else {
        finish_git(&mut git);
        return Err(io::Error::other("git archive: no stderr"));
    };

    let mut tar = match std::process::Command::new("tar")
        .args(["-x", "-f", "-", "-C", dest.to_string_lossy().as_ref()])
        .stdin(std::process::Stdio::from(git_stdout))
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            finish_git(&mut git);
            return Err(e);
        }
    };

    // stderr is drained on its own thread. Waiting for tar first is required —
    // it is what empties the stdout pipe — but git can fill the stderr pipe
    // meanwhile, and a git blocked on stderr never closes stdout, so tar waits
    // for an EOF that never comes and the build hangs.
    let Some(mut tar_stderr) = tar.stderr.take() else {
        finish_git(&mut git);
        let _ = tar.kill();
        let _ = tar.wait();
        return Err(io::Error::other("tar: no stderr"));
    };
    let (tar_status, git_status, git_err, tar_err) = std::thread::scope(|scope| {
        let drain_git = scope.spawn(move || {
            let mut buf = Vec::new();
            let _ = git_stderr.read_to_end(&mut buf);
            buf
        });
        let drain_tar = scope.spawn(move || {
            let mut buf = Vec::new();
            let _ = tar_stderr.read_to_end(&mut buf);
            buf
        });
        let tar_status = tar.wait();
        let git_status = git.wait();
        (
            tar_status,
            git_status,
            drain_git.join().unwrap_or_default(),
            drain_tar.join().unwrap_or_default(),
        )
    });

    // Both messages, rather than a rule for picking one. Every rule tried here
    // was wrong for some case: tar-first lost git's stderr when a bad revision
    // made tar fail on the empty stream it received; a SIGPIPE test is
    // Unix-only; "git said something" misfires because `GIT_TRACE` in the
    // caller's environment makes git write to stderr without failing. The
    // reader wants to know what happened, and when two processes fail together
    // the honest answer names both.
    let tar_status = tar_status?;
    let git_status = git_status?;
    if git_status.success() && tar_status.success() {
        return Ok(());
    }
    let mut parts = Vec::new();
    if !git_status.success() {
        parts.push(format!(
            "git archive: {} {}",
            git_status,
            String::from_utf8_lossy(&git_err).trim()
        ));
    }
    if !tar_status.success() {
        parts.push(format!(
            "tar extract: {} {}",
            tar_status,
            String::from_utf8_lossy(&tar_err).trim()
        ));
    }
    Err(io::Error::other(parts.join("; ")))
}

pub(crate) fn collect_refs(worktree: &Path, sha: &str) -> io::Result<Vec<RefRecord>> {
    let out = safe_exec::git()
        .args(["for-each-ref", "--points-at", sha, "--format=%(refname)"])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Ok(vec![]);
    }
    let now = chrono::Utc::now().to_rfc3339();
    Ok(std::str::from_utf8(&out.stdout)
        .unwrap_or("")
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| RefRecord {
            ref_name: l.to_string(),
            seen_at: now.clone(),
        })
        .collect())
}

pub(crate) fn source_type_from_refs(refs: &[RefRecord]) -> SourceType {
    if refs.iter().any(|r| r.ref_name.starts_with("refs/heads/")) {
        return SourceType::Branch;
    }
    if refs.iter().any(|r| r.ref_name.starts_with("refs/tags/")) {
        return SourceType::Tag;
    }
    if refs.iter().any(|r| {
        r.ref_name.starts_with("refs/pull/") || r.ref_name.starts_with("refs/merge-requests/")
    }) {
        return SourceType::Pr;
    }
    SourceType::Commit
}

pub(crate) fn source_id_from_refs(refs: &[RefRecord]) -> Option<String> {
    for r in refs {
        if let Some(b) = r.ref_name.strip_prefix("refs/heads/") {
            return Some(b.to_string());
        }
    }
    for r in refs {
        if let Some(t) = r.ref_name.strip_prefix("refs/tags/") {
            return Some(t.to_string());
        }
    }
    for r in refs {
        if let Some(rest) = r
            .ref_name
            .strip_prefix("refs/pull/")
            .or_else(|| r.ref_name.strip_prefix("refs/merge-requests/"))
        {
            if let Some(n) = rest.split('/').next() {
                return Some(n.to_string());
            }
        }
    }
    None
}

pub(crate) fn parent_sha(worktree: &Path, sha: &str) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["rev-parse", &format!("{sha}^")])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("no parent"));
    }
    Ok(std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_string())
}
/// fsync every regular file and every directory under `dir`, children before
/// their parent and `dir` itself last, so the rename publishes nothing a crash
/// can lose. `_src` is skipped: it is retired right after the publish and
/// never read from the published dir. Every walk error fails the publish; a
/// file left unsynced would otherwise be published as if it were durable.
pub(crate) fn sync_tree(dir: &Path, publish: &dyn Publish) -> io::Result<()> {
    // Pre-order, not `contents_first`: with `contents_first` walkdir yields a
    // directory's children before `filter_entry` sees the directory, so `_src`
    // would still be walked. Reversed pre-order puts children before parents.
    let mut dirs = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|entry| !(entry.depth() == 1 && entry.file_name() == "_src"))
    {
        let entry = entry.map_err(io::Error::from)?;
        let file_type = entry.file_type();
        if file_type.is_file() {
            publish.sync_file(entry.path())?;
        } else if file_type.is_dir() {
            dirs.push(entry.into_path());
        }
    }
    for path in dirs.iter().rev() {
        publish.sync_dir(path)?;
    }
    Ok(())
}

fn sync_file(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        fs::OpenOptions::new().write(true).open(path)?.sync_all()
    }
    #[cfg(not(windows))]
    fs::File::open(path)?.sync_all()
}

pub(crate) fn update_repo_meta(repo_root: &Path, worktree: &Path, sha: &str) -> io::Result<()> {
    let meta_path = repo_root.join("meta.json");
    // `FileLock` rather than a bare `lock_exclusive()`: an unbounded wait here
    // hangs indexing behind a holder that will never release, which is the
    // freeze PR #475 removed from the registry and left in place here.
    let _lock = ecp_core::registry::FileLock::acquire_exclusive(&repo_root.join(".meta.lock"))?;

    let mut rm = if meta_path.exists() {
        RepoMeta::read(&meta_path)?
    } else {
        RepoMeta {
            version: 1,
            common_dir: git_common_dir_string(worktree)?,
            remote_url: git_remote_url(worktree).ok(),
            aliases: vec![],
            known_refs: Default::default(),
            last_built_sha: None,
            total_size_bytes: 0,
            last_touched: chrono::Utc::now().to_rfc3339(),
        }
    };
    rm.last_built_sha = Some(sha.to_string());
    rm.last_touched = chrono::Utc::now().to_rfc3339();
    rm.total_size_bytes = dir_size(repo_root)?;
    RepoMeta::write_atomic(&meta_path, &rm)?;

    // Sync the global registry. Without this, `contracts --repo @all`,
    // `summary`, and any other Registry-backed reader is blind to repos
    // indexed since the registry was last (manually) rebuilt — `rebuild_from_disk`
    // was the only writer the build path ever touched, and it has zero callers
    // outside the v2 migration probe. Lock order: caller holds per-repo
    // `.meta.lock` (above) → we acquire registry lock here. Single-direction
    // nesting: every other registry writer takes only the registry lock.
    let repo_dir_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::other("repo_root has no final component"))?;
    let home_ecp = repo_root
        .parent()
        .ok_or_else(|| io::Error::other("repo_root has no parent (home_ecp)"))?;
    RegistryFile::upsert_repo_atomic(home_ecp, RepoAlias::from_repo_meta(repo_dir_name, &rm))?;
    Ok(())
}

fn git_common_dir_string(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        // Non-git: store the canonical worktree path as `common_dir`. Mirrors
        // `find_by_path` in `repo_selector.rs`, which also falls back to the
        // canonical worktree path when git is unavailable. Both sides converge
        // on the same string so registry lookup succeeds for nogit repos.
        // `dunce::canonicalize` keeps the path plain on Windows (no `\\?\`
        // verbatim prefix), so the stored `common_dir` never feeds a verbatim
        // path into the build pipeline (`ignore::WalkBuilder` / `git archive`).
        return Ok(dunce::canonicalize(worktree)?
            .to_string_lossy()
            .into_owned());
    }
    let s = std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim();
    let p = std::path::PathBuf::from(s);
    let resolved = if p.is_absolute() { p } else { worktree.join(p) };
    Ok(dunce::canonicalize(resolved)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| s.to_string()))
}

fn git_remote_url(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["remote", "get-url", "origin"])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("no origin remote"));
    }
    Ok(std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_string())
}

fn dir_size(dir: &Path) -> io::Result<u64> {
    // CI-M-followup: tolerant per-entry metadata fetch. The background
    // tantivy writer (CI-B) actively churns `.tmpXXX` segment files inside
    // `publish_dir/tantivy/` — walkdir enumerates them, then a fraction of
    // a millisecond later tantivy renames/deletes the segment for compaction.
    // `metadata()?` on the now-gone path bubbles up an `io::Error` whose
    // walkdir Display format is `IO error for operation on PATH: kind`,
    // which previously surfaced as a `build_l2 failed` subprocess error in
    // Linux + macOS CI (Windows happened to win the race). `total_size_bytes`
    // is an advisory stats field — undercounting by a few transient temp
    // files is acceptable; treating those misses as a fatal build error is
    // not.
    let mut total = 0;
    for e in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        if e.file_type().is_file() {
            if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    Ok(total)
}

/// A one-commit git worktree and an ecp repo root beside it, both in one
/// temp dir. Builds run through the `_at` entry points with this root, so no
/// test here reads or sets `HOME` / `ECP_HOME`.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) struct Fx {
        pub tmp: tempfile::TempDir,
        pub wt: PathBuf,
        pub repo_root: PathBuf,
        pub commits: PathBuf,
        pub sha: [u8; 20],
        pub base: String,
    }

    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git available");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    pub(crate) fn fx() -> Fx {
        let tmp = tempfile::TempDir::new().unwrap();
        let wt = tmp.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q"]);
        fs::write(wt.join("a.py"), "def a():\n    return 1\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "one"]);
        let sha = parse_sha(&git(&wt, &["rev-parse", "HEAD"])).unwrap();
        let repo_root = tmp.path().join("home").join("repo");
        let commits = repo_root.join("commits");
        fs::create_dir_all(&commits).unwrap();
        let base = pick_dirname(&wt, &hex::encode(sha)).unwrap();
        Fx {
            tmp,
            wt,
            repo_root,
            commits,
            sha,
            base,
        }
    }

    pub(crate) fn no_wait(_: &Holder) {
        panic!("nothing else holds this SHA");
    }

    /// `build_l2_at` with the tantivy writer joined, so the temp dir can go.
    pub(crate) fn build_at(fx: &Fx, repo_root: &Path) -> BuildResult {
        let mut r = build_l2_at(&fx.wt, &fx.sha, repo_root, no_wait).unwrap();
        r.join_background();
        r
    }

    pub(crate) fn meta(sha_hex: &str, worktree: &Path, fingerprint: &str) -> CommitBuildMeta {
        CommitBuildMeta {
            version: 1,
            sha: sha_hex.to_string(),
            source_type: SourceType::Branch,
            source_id: Some("main".to_string()),
            built_from_worktree: worktree.to_string_lossy().into(),
            built_at: "2026-10-25T00:00:00Z".to_string(),
            parent_sha: None,
            node_count: 0,
            embedding_status: EmbeddingStatus::None,
            refs_at_build: vec![],
            refs_seen_since: vec![],
            builder_fingerprint: Some(fingerprint.to_string()),
            binary_commit_sha: None,
        }
    }

    /// A dir an older binary published: a stale fingerprint, so nothing
    /// attaches to it.
    pub(crate) fn stale_build(fx: &Fx, name: &str) -> PathBuf {
        let dir = fx.commits.join(name);
        fs::create_dir_all(&dir).unwrap();
        CommitBuildMeta::write_atomic(
            &dir.join("meta.json"),
            &meta(&hex::encode(fx.sha), &fx.wt, "v0.0.0+stale"),
        )
        .unwrap();
        fs::write(dir.join("graph.bin"), b"old").unwrap();
        dir
    }

    pub(crate) fn current_name(fx: &Fx) -> Option<String> {
        CommitIndex::scan(&fx.commits)
            .unwrap()
            .find(&fx.sha)
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::commit_lookup::StagingDir;
    use ecp_core::registry::CommitDirName;
    use std::cell::RefCell;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn listing(names: &[String]) -> CommitDirListing {
        CommitDirListing::from_entries(
            Path::new("commits"),
            names.iter().map(|n| Ok((n.clone(), true))),
        )
    }

    #[test]
    fn test_next_publish_name_no_same_sha_bare() {
        assert_eq!(
            next_publish_name("commit__x", &[], 5, 1, 0).unwrap(),
            "commit__x"
        );
    }

    /// The clock stepped back below the newest generation: the name still
    /// rises. Equal clock and newest rise by one.
    #[test]
    fn test_next_publish_name_now_behind_newest_rises() {
        let gen = |ts| {
            Some(Generation {
                timestamp_ms: ts,
                pid: 9,
                counter: 9,
            })
        };
        assert_eq!(
            next_publish_name("b", &[gen(2_000_000), None], 1_000, 1, 0).unwrap(),
            "b.gen.2000001.1.0"
        );
        assert_eq!(
            next_publish_name("b", &[gen(1_000)], 1_000, 1, 0).unwrap(),
            "b.gen.1001.1.0"
        );
        assert_eq!(
            next_publish_name("b", &[None], 1_000, 1, 0).unwrap(),
            "b.gen.1000.1.0",
            "a bare sibling only forces a generation"
        );
    }

    #[test]
    fn test_next_publish_name_u64_max_refuses() {
        let full = Some(Generation {
            timestamp_ms: u64::MAX,
            pid: 0,
            counter: 0,
        });
        assert!(next_publish_name("b", &[full], 1_000, 1, 0).is_err());
    }

    /// Moved from `publish_dir_for`: a sweep that kept the `.gen` dir and
    /// dropped the bare one must not hand the bare name out again, or the new
    /// build ranks below the older dir it replaces.
    #[test]
    fn test_choose_publish_name_generation_sibling_ranks_above_it() {
        let older = format!("branch_main__{SHA}.gen.1730000000000.42.7");
        let other = format!("branch_main__{}", "f".repeat(40));
        let l = listing(&[older.clone(), other]);
        let sha = parse_sha(SHA).unwrap();
        let name = choose_publish_name(&l, &sha, &format!("branch_main__{SHA}"), 5).unwrap();
        let parsed = CommitDirName::parse(&name).unwrap();
        assert_eq!(parsed.sha, sha);
        assert!(parsed.generation > CommitDirName::parse(&older).unwrap().generation);
    }

    /// Moved from `publish_dir_for`: `commit__<sha>` exists from before a
    /// branch pointed at the SHA; the branch build takes a generation.
    #[test]
    fn test_choose_publish_name_other_prefix_bare_sibling_gets_generation() {
        let l = listing(&[format!("commit__{SHA}")]);
        let name = choose_publish_name(
            &l,
            &parse_sha(SHA).unwrap(),
            &format!("branch_main__{SHA}"),
            5,
        )
        .unwrap();
        assert!(
            CommitDirName::parse(&name).unwrap().generation.is_some(),
            "{name}"
        );
    }

    #[test]
    fn test_choose_publish_name_no_same_sha_dir_bare() {
        let l = listing(&[format!("branch_main__{}.gen.1.2.3", "f".repeat(40))]);
        let base = format!("branch_main__{SHA}");
        assert_eq!(
            choose_publish_name(&l, &parse_sha(SHA).unwrap(), &base, 5).unwrap(),
            base
        );
    }

    /// Replaces `publish_dir_for`'s "unreadable parent gets a generation":
    /// a listing that missed an entry may have missed the newest generation,
    /// so no name is chosen at all.
    #[test]
    fn test_choose_publish_name_incomplete_listing_refuses() {
        let l = CommitDirListing::from_entries(
            Path::new("commits"),
            [Err(io::Error::other("entry vanished"))],
        );
        let err = choose_publish_name(&l, &parse_sha(SHA).unwrap(), "b", 5).unwrap_err();
        assert!(err.to_string().contains("incomplete"), "{err}");
    }

    #[test]
    fn test_parse_sha_uppercase_same_bytes_and_lock_path() {
        let lower = parse_sha(SHA).unwrap();
        let upper = parse_sha(&SHA.to_uppercase()).unwrap();
        assert_eq!(lower, upper);
        let commits = Path::new("commits");
        assert_eq!(
            sha_lock_path(commits, &upper),
            sha_lock_path(commits, &lower)
        );
        assert!(parse_sha("xyz").is_err());
        assert!(parse_sha(&"g".repeat(40)).is_err());
    }

    /// Two refs at one SHA used to take two locks (one per ref-derived dir
    /// name). Both entries now key the lock by the SHA only.
    #[test]
    fn test_acquire_sha_lock_two_refs_same_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sha = parse_sha(SHA).unwrap();
        let held = acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, no_wait).unwrap();
        let err = acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, |_| {})
            .err()
            .expect("the second builder of one SHA must wait, whatever its ref");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(held);
    }

    /// `SHA` with its first byte kept and the rest changed: the same stripe.
    fn same_stripe(sha: &[u8; 20]) -> [u8; 20] {
        let mut other = *sha;
        other[1..].iter_mut().for_each(|b| *b ^= 0xff);
        other
    }

    #[test]
    fn test_acquire_sha_lock_busy_reports_holder_slot_phase() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sha = parse_sha(SHA).unwrap();
        let held = acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, no_wait).unwrap();
        let mut calls = Vec::new();
        let err = acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, |h| {
            calls.push(h.to_string())
        })
        .err()
        .expect("a held lock with no budget ends in an error");
        let pid = std::process::id();
        assert_eq!(
            calls,
            vec![format!(
                "sha=01234567 holder={pid} role=build phase=slot-wait"
            )]
        );
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        let text = err.to_string();
        assert!(
            text.starts_with("l2.wait timeout sha=01234567")
                && text.contains(&format!("holder={pid}"))
                && text.contains("phase=slot-wait"),
            "{text}"
        );
        drop(held);
    }

    #[test]
    fn test_acquire_sha_lock_busy_reports_holder_building_phase() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sha = parse_sha(SHA).unwrap();
        let staging = staging_dir_name(&format!("branch_main__{SHA}"), std::process::id(), 0);
        fs::create_dir_all(tmp.path().join(&staging)).unwrap();
        let held = acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, no_wait).unwrap();
        let mut seen = String::new();
        let err = acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, |h| seen = h.to_string())
            .err()
            .unwrap();
        assert!(
            seen.contains(&format!("phase=building staging={staging} age=")),
            "{seen}"
        );
        assert!(err.to_string().contains("phase=building"), "{err}");
        drop(held);
    }

    /// Two SHAs of one stripe build one after the other. The waiter learns
    /// that the stripe is held for another SHA, and never reads that PID as
    /// its own SHA's builder.
    #[test]
    fn test_acquire_sha_lock_same_stripe_other_sha_waits_names_other_sha() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sha = parse_sha(SHA).unwrap();
        let other = same_stripe(&sha);
        assert_eq!(
            sha_lock_path(tmp.path(), &sha),
            sha_lock_path(tmp.path(), &other)
        );
        let held = acquire_sha_lock(tmp.path(), &other, Duration::ZERO, no_wait).unwrap();
        let mut seen = String::new();
        let err = acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, |h| seen = h.to_string())
            .err()
            .expect("one stripe, one holder");
        let pid = std::process::id();
        let other8 = &hex::encode(other)[..8];
        assert_eq!(
            seen,
            format!(
                "sha=01234567 stripe_holder={pid} role=build phase=other-sha held_for={other8}"
            )
        );
        assert!(!seen.contains(" holder="), "{seen}");
        assert!(err.to_string().contains("phase=other-sha"), "{err}");
        drop(held);
        drop(acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, no_wait).unwrap());
    }

    /// Two stripes are two locks: SHAs with different first bytes build at
    /// once.
    #[test]
    fn test_acquire_sha_lock_two_stripes_held_at_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sha = parse_sha(SHA).unwrap();
        let mut other = sha;
        other[0] ^= 0xff;
        let first = acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, no_wait).unwrap();
        let second = acquire_sha_lock(tmp.path(), &other, Duration::ZERO, no_wait).unwrap();
        drop((first, second));
    }

    /// A holder that goes away frees the stripe and leaves its file: the
    /// lock file is never deleted, so no waiter holds a descriptor of an
    /// unlinked inode.
    #[test]
    fn test_acquire_sha_lock_holder_gone_lock_free_file_kept() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sha = parse_sha(SHA).unwrap();
        let path = sha_lock_path(tmp.path(), &sha);
        drop(acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, no_wait).unwrap());
        assert!(path.is_file());
        assert!(FileLock::try_existing(&path).unwrap().is_some());
        drop(acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, no_wait).unwrap());
    }

    /// However many SHAs a repo builds, `.locks` holds one lock and one owner
    /// sidecar per first byte: 512 files at most.
    #[test]
    fn test_acquire_sha_lock_many_shas_locks_dir_bounded() {
        let tmp = tempfile::TempDir::new().unwrap();
        for i in 0..1024u32 {
            let mut sha = [0u8; 20];
            sha[..4].copy_from_slice(&i.wrapping_mul(2_654_435_761).to_be_bytes());
            sha[19] = i as u8;
            drop(acquire_sha_lock(tmp.path(), &sha, Duration::ZERO, no_wait).unwrap());
        }
        let files = fs::read_dir(tmp.path().join(LOCKS_DIR)).unwrap().count();
        assert!(files <= 512, "{files} files in .locks");
        let listing = CommitDirListing::read(tmp.path()).unwrap();
        assert!(listing.dirs.is_empty() && listing.unparsed.is_empty());
    }

    #[test]
    fn test_sync_tree_children_before_parent_src_skipped() {
        #[derive(Default)]
        struct Order(RefCell<Vec<PathBuf>>);
        impl Publish for Order {
            fn sync_file(&self, path: &Path) -> io::Result<()> {
                self.0.borrow_mut().push(path.to_path_buf());
                Ok(())
            }
            fn sync_dir(&self, path: &Path) -> io::Result<()> {
                self.0.borrow_mut().push(path.to_path_buf());
                Ok(())
            }
            fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
                unreachable!()
            }
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let staging = tmp.path().join("s");
        fs::create_dir_all(staging.join("tantivy")).unwrap();
        fs::create_dir_all(staging.join("_src").join("pkg")).unwrap();
        fs::write(staging.join("graph.bin"), b"g").unwrap();
        fs::write(staging.join("tantivy").join("seg"), b"t").unwrap();
        fs::write(staging.join("_src").join("pkg").join("a.py"), b"a").unwrap();
        let order = Order::default();
        sync_tree(&staging, &order).unwrap();
        let seen = order.0.into_inner();
        assert_eq!(
            seen.last(),
            Some(&staging),
            "the staging dir is synced last"
        );
        let at = |p: PathBuf| seen.iter().position(|s| *s == p).unwrap();
        assert!(at(staging.join("tantivy").join("seg")) < at(staging.join("tantivy")));
        assert!(seen.iter().all(|p| !p.starts_with(staging.join("_src"))));
        assert!(seen.contains(&staging.join("graph.bin")));
    }

    #[cfg(unix)]
    #[test]
    fn test_sync_tree_unreadable_subdir_publish_fails() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let locked = tmp.path().join("s").join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root ignores mode bits, so the walk succeeds and the case is unreachable.
        let unreachable = fs::read_dir(&locked).is_ok();
        let result = sync_tree(&tmp.path().join("s"), &DurablePublish);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if unreachable {
            return;
        }
        assert!(
            result.is_err(),
            "an unwalkable subdir was published unsynced"
        );
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Op {
        File(PathBuf),
        Dir(PathBuf),
        Rename { from: PathBuf, sidecars: bool },
        Commits { lock_busy: bool },
    }

    /// Records each publish step, and can fail a file fsync, the rename or
    /// the fsync of `commits/`.
    struct Recorder {
        ops: RefCell<Vec<Op>>,
        commits: PathBuf,
        lock: PathBuf,
        fail_sync_file: bool,
        fail_rename: bool,
        fail_commits_sync: bool,
    }

    impl Recorder {
        fn new(fx: &Fx) -> Self {
            Self {
                ops: RefCell::default(),
                commits: fx.commits.clone(),
                lock: sha_lock_path(&fx.commits, &fx.sha),
                fail_sync_file: false,
                fail_rename: false,
                fail_commits_sync: false,
            }
        }
    }

    impl Publish for Recorder {
        fn sync_file(&self, path: &Path) -> io::Result<()> {
            self.ops.borrow_mut().push(Op::File(path.to_path_buf()));
            if self.fail_sync_file {
                return Err(io::Error::other("injected file fsync failure"));
            }
            DurablePublish.sync_file(path)
        }

        fn sync_dir(&self, path: &Path) -> io::Result<()> {
            if path != self.commits {
                self.ops.borrow_mut().push(Op::Dir(path.to_path_buf()));
                return DurablePublish.sync_dir(path);
            }
            let lock_busy = matches!(
                FileLock::try_existing(&self.lock),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock
            );
            self.ops.borrow_mut().push(Op::Commits { lock_busy });
            if self.fail_commits_sync {
                return Err(io::Error::other("injected fsync failure"));
            }
            DurablePublish.sync_dir(path)
        }

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            let graph = from.join("graph.bin");
            let sidecars = crate::auto_ensure::builder_fingerprint_sidecar_path(&graph).is_file()
                && crate::auto_ensure::compatible_version_sidecar_path(&graph).is_file()
                && from.join("meta.json").is_file();
            self.ops.borrow_mut().push(Op::Rename {
                from: from.to_path_buf(),
                sidecars,
            });
            if self.fail_rename {
                // EIO on Unix; ERROR_ACCESS_DENIED, an open handle, on Windows.
                return Err(io::Error::from_raw_os_error(5));
            }
            DurablePublish.rename(from, to)
        }
    }

    fn locked(fx: &Fx) -> FileLock {
        acquire_sha_lock(&fx.commits, &fx.sha, Duration::ZERO, no_wait).unwrap()
    }

    fn lock_is_free(fx: &Fx) -> bool {
        FileLock::try_existing(&sha_lock_path(&fx.commits, &fx.sha))
            .is_ok_and(|probe| probe.is_some())
    }

    /// Files, then their dirs bottom-up, the staging dir last, the rename,
    /// then `commits/` while the SHA lock is still held. The build-time
    /// sidecars are in staging when it is renamed.
    #[test]
    fn test_build_inside_locked_publish_order() {
        let fx = fx();
        let rec = Recorder::new(&fx);
        let mut r = build_inside_locked(
            &fx.wt,
            &fx.wt,
            &fx.sha,
            &fx.repo_root,
            &fx.base,
            locked(&fx),
            &rec,
        )
        .unwrap();
        r.join_background();
        let ops = rec.ops.into_inner();
        let at = ops
            .iter()
            .position(|o| matches!(o, Op::Rename { .. }))
            .expect("a rename");
        let Op::Rename {
            from: staging,
            sidecars,
        } = ops[at].clone()
        else {
            unreachable!()
        };
        assert!(sidecars, "sidecars must publish with graph.bin");
        assert_eq!(
            ops[at - 1],
            Op::Dir(staging.clone()),
            "staging is synced last"
        );
        assert_eq!(&ops[at + 1..], &[Op::Commits { lock_busy: true }][..]);
        for (i, op) in ops[..at].iter().enumerate() {
            let Op::File(file) = op else { continue };
            for (j, later) in ops[..at].iter().enumerate() {
                if let Op::Dir(dir) = later {
                    if file.starts_with(dir) {
                        assert!(j > i, "{dir:?} synced before its file {file:?}");
                    }
                }
            }
        }
        assert!(ops.contains(&Op::File(staging.join("graph.bin"))));
        assert_eq!(
            r.commit_dir,
            fx.commits.join(&fx.base),
            "first build is bare"
        );
        assert!(lock_is_free(&fx), "the SHA lock is released on return");
    }

    /// A rename refused (an open handle on Windows): the error returns, the
    /// previous build stays current, the attempt's staging is retired and the
    /// SHA lock is free.
    #[test]
    fn test_build_inside_locked_rename_fails_previous_current_lock_free() {
        let fx = fx();
        stale_build(&fx, &fx.base);
        let mut rec = Recorder::new(&fx);
        rec.fail_rename = true;
        let result = build_inside_locked(
            &fx.wt,
            &fx.wt,
            &fx.sha,
            &fx.repo_root,
            &fx.base,
            locked(&fx),
            &rec,
        );
        assert!(result.is_err());
        assert_eq!(current_name(&fx), Some(fx.base.clone()));
        let listing = CommitDirListing::read(&fx.commits).unwrap();
        assert!(listing.staging.is_empty(), "{:?}", listing.staging);
        assert_eq!(listing.dirs.len(), 1);
        assert!(lock_is_free(&fx));
    }

    /// The rename happened and the fsync of `commits/` failed: the caller
    /// hears of it, and the published dir is not taken back (gc keeps the
    /// previous generation until its own fsync succeeds). Every step after
    /// the rename still ran: `_src` is gone, `.head_sha` and the repo meta
    /// are written.
    #[test]
    fn test_build_inside_locked_commits_fsync_fails_error_published_kept() {
        let fx = fx();
        let old = stale_build(&fx, &fx.base);
        // An untracked file makes the build read the commit from `_src`.
        fs::write(fx.wt.join("untracked.py"), "x = 1\n").unwrap();
        let mut rec = Recorder::new(&fx);
        rec.fail_commits_sync = true;
        let err = build_inside_locked(
            &fx.wt,
            &fx.wt,
            &fx.sha,
            &fx.repo_root,
            &fx.base,
            locked(&fx),
            &rec,
        )
        .err()
        .expect("the failed fsync is reported");
        assert!(err.to_string().contains("fsync"), "{err}");
        let current = current_name(&fx).unwrap();
        assert_ne!(current, fx.base, "the new generation is current");
        assert!(old.exists());
        assert!(lock_is_free(&fx));
        let published = fx.commits.join(&current);
        assert!(!published.join("_src").exists(), "_src was not retired");
        let head_sha = crate::auto_ensure::head_sha_sidecar_path(&published.join("graph.bin"));
        assert_eq!(
            fs::read_to_string(head_sha).unwrap().trim(),
            hex::encode(fx.sha)
        );
        assert!(
            fx.repo_root.join("meta.json").is_file(),
            "repo meta not updated"
        );
    }

    /// A publish step fails while the lock is held: the error returns, the
    /// attempt's staging is retired, nothing is published and the SHA lock is
    /// free.
    #[test]
    fn test_build_inside_locked_sync_file_fails_lock_free_no_staging() {
        let fx = fx();
        let mut rec = Recorder::new(&fx);
        rec.fail_sync_file = true;
        let err = build_inside_locked(
            &fx.wt,
            &fx.wt,
            &fx.sha,
            &fx.repo_root,
            &fx.base,
            locked(&fx),
            &rec,
        )
        .err()
        .expect("the failed fsync is reported");
        assert!(err.to_string().contains("injected"), "{err}");
        let listing = CommitDirListing::read(&fx.commits).unwrap();
        assert!(
            listing.staging.is_empty() && listing.dirs.is_empty(),
            "{listing:?}"
        );
        assert!(lock_is_free(&fx));
    }

    /// The wall clock steps back an hour between two publishes of one SHA.
    /// The second name still rises above the first (`max(now, newest + 1)`),
    /// so readers take the second build.
    #[test]
    fn test_choose_publish_name_clock_steps_back_second_publish_ranks_above_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        let commits = tmp.path();
        let sha = parse_sha(SHA).unwrap();
        let base = format!("commit__{SHA}");
        fs::create_dir(commits.join(&base)).unwrap();
        let publish_at = |now_ms| {
            let listing = CommitDirListing::read(commits).unwrap();
            let name = choose_publish_name(&listing, &sha, &base, now_ms).unwrap();
            fs::create_dir(commits.join(&name)).unwrap();
            name
        };
        let first = publish_at(5_000_000);
        let second = publish_at(5_000_000 - 3_600_000);
        let ts = |name: &str| {
            CommitDirName::parse(name)
                .unwrap()
                .generation
                .unwrap()
                .timestamp_ms
        };
        assert_eq!(ts(&first), 5_000_000);
        assert_eq!(ts(&second), 5_000_001);
        assert_eq!(
            CommitIndex::scan(commits).unwrap().find(&sha),
            Some(second.as_str())
        );
    }

    /// The process stopped after the rename and before `.head_sha` and the
    /// repo meta. The next build of the SHA attaches to the published dir,
    /// back-fills `.head_sha`, and builds nothing.
    #[test]
    fn test_build_l2_at_published_without_head_sha_attaches_and_backfills() {
        let fx = fx();
        let first = build_at(&fx, &fx.repo_root);
        let head_sha =
            crate::auto_ensure::head_sha_sidecar_path(&first.commit_dir.join("graph.bin"));
        fs::remove_file(&head_sha).unwrap();
        fs::remove_file(fx.repo_root.join("meta.json")).unwrap();

        let second = build_l2_at(&fx.wt, &fx.sha, &fx.repo_root, no_wait).unwrap();

        assert_eq!(second.commit_dir, first.commit_dir);
        assert!(second.tantivy_handle.is_none(), "attached, not built");
        assert_eq!(
            fs::read_to_string(&head_sha).unwrap().trim(),
            hex::encode(fx.sha)
        );
        assert_eq!(CommitDirListing::read(&fx.commits).unwrap().dirs.len(), 1);
    }

    /// gc holds the stripe: a builder of the SHA waits and reports gc as the
    /// holder. gc retires the SHA's only dir and lets go; the builder checks
    /// the publish state again under the lock, finds nothing and builds.
    #[test]
    fn test_build_l2_at_gc_holds_builder_waits_then_rechecks_and_builds() {
        use crate::admin::gc::{sweep_sha_lock, ShaLock};
        let fx = fx();
        let old = stale_build(&fx, &fx.base);
        let ShaLock::Held(gc) = sweep_sha_lock(&fx.commits, &fx.sha, false) else {
            panic!("nothing else holds the stripe");
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                build_l2_at(&fx.wt, &fx.sha, &fx.repo_root, move |h: &Holder| {
                    tx.send(h.to_string()).unwrap()
                })
            });
            let line = rx.recv().expect("the builder reports its wait");
            assert!(line.ends_with("role=gc phase=gc"), "{line}");
            ecp_core::registry::retire_dir(&old).unwrap();
            drop(gc);
            let mut r = waiter.join().unwrap().unwrap();
            assert!(r.tantivy_handle.is_some(), "built, not attached");
            r.join_background();
            assert_eq!(
                r.commit_dir,
                fx.commits.join(&fx.base),
                "nothing of the SHA was left, so the name is bare"
            );
        });
    }

    /// Crash before the rename: the previous build stays current and the
    /// abandoned per-attempt staging is never a candidate. The next build of
    /// the SHA retires the staging and publishes above the previous build.
    #[test]
    fn test_build_l2_at_abandoned_new_staging_old_build_current_then_retired() {
        let fx = fx();
        let old = stale_build(&fx, &fx.base);
        let junk = fx.commits.join(staging_dir_name(&fx.base, 999_999, 0));
        fs::create_dir_all(&junk).unwrap();
        fs::write(junk.join("graph.bin"), b"junk").unwrap();
        assert_eq!(current_name(&fx), Some(fx.base.clone()));

        let r = build_at(&fx, &fx.repo_root);

        assert!(!junk.exists(), "abandoned staging must be retired");
        let listing = CommitDirListing::read(&fx.commits).unwrap();
        assert!(listing.staging.is_empty());
        let new_name = r.commit_dir.file_name().unwrap().to_str().unwrap();
        assert_eq!(current_name(&fx).as_deref(), Some(new_name));
        assert!(old.exists(), "the previous build is left to gc");
    }

    fn tree(dir: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        walkdir::WalkDir::new(dir)
            .sort_by_file_name()
            .into_iter()
            .map(|e| {
                let e = e.unwrap();
                let rel = e.path().strip_prefix(dir).unwrap().to_path_buf();
                // A held lock file cannot be read on Windows; its presence is enough.
                let body = (e.file_type().is_file() && e.file_name() != ".build.lock")
                    .then(|| fs::read(e.path()).unwrap());
                (rel, body)
            })
            .collect()
    }

    /// An older binary is mid-build in `<base>.building/`. A current build of
    /// the SHA neither opens, reuses nor removes it.
    #[test]
    fn test_build_l2_at_old_style_staging_untouched() {
        let fx = fx();
        let legacy = fx.commits.join(format!("{}.building", fx.base));
        fs::create_dir_all(legacy.join("_src")).unwrap();
        fs::write(legacy.join("graph.bin"), b"half").unwrap();
        fs::write(legacy.join("_src").join("x.py"), b"x = 1\n").unwrap();
        let held = FileLock::acquire_exclusive(&legacy.join(".build.lock")).unwrap();
        let before = tree(&legacy);

        build_at(&fx, &fx.repo_root);

        assert_eq!(tree(&legacy), before);
        drop(held);
    }

    /// A builder waiting on the SHA attaches to what the holder published
    /// during the wait instead of building it again.
    #[test]
    fn test_build_l2_at_waiter_attaches_after_holder_publishes() {
        let fx = fx();
        let elsewhere = fx.tmp.path().join("home").join("elsewhere");
        let built = build_at(&fx, &elsewhere);
        let held = locked(&fx);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                build_l2_at(&fx.wt, &fx.sha, &fx.repo_root, move |h: &Holder| {
                    tx.send(h.to_string()).unwrap()
                })
            });
            let line = rx.recv().expect("the waiter reports its wait");
            assert!(
                line.contains(&format!("holder={}", std::process::id())),
                "{line}"
            );
            let published = fx.commits.join(built.commit_dir.file_name().unwrap());
            fs::rename(&built.commit_dir, &published).unwrap();
            drop(held);
            let r = waiter.join().unwrap().unwrap();
            assert_eq!(r.commit_dir, published);
            assert!(r.tantivy_handle.is_none(), "attached, not built");
        });
        assert_eq!(CommitDirListing::read(&fx.commits).unwrap().dirs.len(), 1);
    }

    /// Two refs at one SHA: the second build finds the first and publishes
    /// nothing.
    #[test]
    fn test_build_l2_at_two_refs_same_sha_one_publish() {
        let fx = fx();
        git(&fx.wt, &["branch", "other"]);
        let first = build_at(&fx, &fx.repo_root);
        git(&fx.wt, &["checkout", "-q", "other"]);
        let second = build_at(&fx, &fx.repo_root);
        assert_eq!(first.commit_dir, second.commit_dir);
        assert_eq!(CommitDirListing::read(&fx.commits).unwrap().dirs.len(), 1);
    }

    #[test]
    fn test_staging_dir_name_parses_back() {
        let name = staging_dir_name(&format!("commit__{SHA}"), 7, 3);
        let StagingDir { sha, shape, .. } =
            crate::commit_lookup::parse_staging_name(&name).unwrap();
        assert_eq!(sha, parse_sha(SHA).unwrap());
        assert_eq!(shape, StagingShape::PerAttempt { pid: 7, attempt: 3 });
    }

    /// The publishing process must see its own build on its next lookup. A
    /// coarse-mtime filesystem can leave `commits/` with the mtime the cache
    /// stored, so only the explicit drop after the rename guarantees that.
    #[test]
    fn test_publish_staging_drops_scan_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let commits = tmp.path().join("commits");
        fs::create_dir(&commits).unwrap();
        let sha = parse_sha(SHA).unwrap();
        let base = format!("commit__{SHA}");
        let settled =
            filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(60));
        filetime::set_file_mtime(&commits, settled).unwrap();
        let (wall, mono) = (SystemTime::now(), std::time::Instant::now());
        let before = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        assert!(before.find(&sha).is_none());

        let staging = commits.join(staging_dir_name(&base, 1, 0));
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("graph.bin"), b"g").unwrap();
        publish_staging(&staging, &commits, &base, &DurablePublish).unwrap();
        filetime::set_file_mtime(&commits, settled).unwrap();

        let after = CommitIndex::scan_cached_at(&commits, wall, mono).unwrap();
        assert_eq!(after.find(&sha), Some(base.as_str()));
    }
}

/// `git_archive_to` is the widest path a scanned repository's filter driver
/// can reach, so it gets its own guard rather than relying on the one over
/// `BaselineCheckout`. `process` is set alongside `smudge` on purpose: it takes
/// precedence, so a fix covering only `smudge` and `clean` leaves this red.
#[cfg(all(test, unix))]
mod archive_filter_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git available");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn git_archive_does_not_run_a_repo_defined_filter_driver() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let dest = tmp.path().join("dest");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let marker = tmp.path().join("archive-filter-ran");
        let script = tmp.path().join("filter.sh");
        // A filter reads the blob on stdin and writes the converted bytes to
        // stdout, so the passthrough is a bare `cat`.
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'executed' > '{}'\ncat\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        git(&repo, &["init", "-q", "."]);
        git(&repo, &["config", "user.email", "t@example.invalid"]);
        git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join(".gitattributes"), "payload.txt filter=evil\n").unwrap();
        fs::write(repo.join("payload.txt"), "content\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "one"]);
        let driver = script.to_str().unwrap();
        git(&repo, &["config", "filter.evil.smudge", driver]);
        git(&repo, &["config", "filter.evil.clean", driver]);
        git(&repo, &["config", "filter.evil.process", driver]);

        let sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        super::git_archive_to(&repo, &sha, &dest).expect("archive must still extract");

        assert!(
            !marker.exists(),
            "a filter driver from the scanned repo ran during git archive: {}",
            fs::read_to_string(&marker).unwrap_or_default()
        );
        assert_eq!(
            fs::read_to_string(dest.join("payload.txt")).unwrap(),
            "content\n",
            "the extracted tree must carry the content as stored"
        );
    }
}

/// `update_repo_meta` used to wait for `.meta.lock` with no deadline, so a
/// holder that never released stopped indexing for good rather than reporting
/// anything. Held here from a second descriptor in this process: `flock` is
/// scoped to the open file description, so two `open` calls conflict even
/// inside one process.
///
/// The elapsed bound is the assertion that matters. Against the previous code
/// this test does not fail, it hangs — which the harness reports as a timeout
/// rather than as this message, so the message says what to look for.
#[cfg(test)]
mod meta_lock_tests {
    use super::*;

    #[test]
    fn update_repo_meta_reports_a_busy_lock_instead_of_waiting_for_ever() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo__deadbeef");
        std::fs::create_dir_all(&repo_root).unwrap();
        let held =
            ecp_core::registry::FileLock::acquire_exclusive(&repo_root.join(".meta.lock")).unwrap();

        let start = std::time::Instant::now();
        let result = update_repo_meta(&repo_root, tmp.path(), &"a".repeat(40));
        let waited = start.elapsed();

        assert!(
            result.is_err(),
            "a lock nobody releases has to end as an error"
        );
        // Bounded on both sides. An upper bound of a minute also passes for a
        // caller that waits fifty-nine seconds, which is not the five this site
        // asks for; a lower bound alone passes for one that never waited.
        assert!(
            waited >= std::time::Duration::from_secs(4),
            "the wait ended before the five seconds this site asks for: {waited:?}"
        );
        assert!(
            waited < std::time::Duration::from_secs(15),
            "if this reads as a harness timeout rather than this assertion, the \
             wait is unbounded again: {waited:?}"
        );
        drop(held);
    }
}

/// The streaming rewrite put two processes and three pipes where there had been
/// one buffered call, and the failure paths are where that shows.
#[cfg(all(test, unix))]
mod archive_process_tests {
    use super::*;

    /// `GIT_TRACE` is process-global, so the two tests that set it take turns.
    /// Without this one test's trace setting leaks into whatever else the
    /// harness happens to be running on another thread.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Sets `GIT_TRACE` for as long as it is held, and clears it on the way
    /// out — including on a panic, which a bare `remove_var` after the
    /// assertions would skip.
    struct GitTrace(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl GitTrace {
        fn on() -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::set_var("GIT_TRACE", "1");
            Self(guard)
        }
    }

    impl Drop for GitTrace {
        fn drop(&mut self) {
            std::env::remove_var("GIT_TRACE");
        }
    }

    fn one_commit_repo(dir: &std::path::Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            vec!["init", "-q", "."],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "t"],
        ] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(dir.join("a.py"), "def a():\n    return 1\n").unwrap();
        // Larger than a pipe buffer, so git actually blocks on the pipe rather
        // than finishing before tar is even scheduled — which is the only state
        // in which the ordering these tests check is observable.
        std::fs::write(dir.join("bulk.txt"), "x".repeat(1_000_000)).unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "one"]] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success());
        }
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    /// tar refuses a destination that does not exist. The call has to come back
    /// with tar's failure — not git's SIGPIPE, which is the consequence — and it
    /// has to come back at all: an early return that dropped the `Child` would
    /// leave `git archive` running with nobody to reap it.
    #[test]
    fn a_failing_tar_returns_its_own_error_and_does_not_hang() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let sha = one_commit_repo(&repo);

        // Trace output on a run where tar is the one that fails: the error has
        // to name tar, not the process that merely printed the most.
        let _trace = GitTrace::on();
        let start = std::time::Instant::now();
        let err = git_archive_to(&repo, &sha, &tmp.path().join("no-such-dir"))
            .expect_err("tar cannot extract into a directory that is not there");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "the call must return, not wait on a child nobody reaps: {:?}",
            start.elapsed()
        );
        assert!(
            err.to_string().contains("tar extract"),
            "tar's failure has to be named: {err}"
        );
    }

    /// A revision that is not a tree makes git fail while tar succeeds on an
    /// empty stream, so this is the case where git's stderr is the only thing
    /// that says what went wrong.
    ///
    /// `GIT_TRACE` is set on purpose. git then writes to stderr without
    /// failing, which is what broke an earlier attempt to pick the causal
    /// process by "did git say anything" — the caller's environment reaches
    /// this subprocess, and a rule that reads trace output as an error would
    /// report git for a failure tar caused.
    #[test]
    fn a_failing_git_reports_what_git_said() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        one_commit_repo(&repo);
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let _trace = GitTrace::on();
        let err = git_archive_to(&repo, &"0".repeat(40), &dest)
            .expect_err("an all-zero sha is not a tree");
        assert!(
            err.to_string().contains("git archive"),
            "expected git's failure: {err}"
        );
        assert!(
            err.to_string().contains("not a tree object"),
            "git's stderr has to survive, or the error names nothing: {err}"
        );
    }
}

#[cfg(test)]
mod worktree_root_tests {
    use super::*;
    use ecp_core::registry::{EmbeddingStatus, SourceType};

    fn meta(built_from_worktree: &str) -> CommitBuildMeta {
        CommitBuildMeta {
            version: 1,
            sha: "0".repeat(40),
            source_type: SourceType::Branch,
            source_id: None,
            built_from_worktree: built_from_worktree.into(),
            built_at: String::new(),
            parent_sha: None,
            node_count: 0,
            embedding_status: EmbeddingStatus::None,
            refs_at_build: vec![],
            refs_seen_since: vec![],
            builder_fingerprint: Some(BUILDER_FINGERPRINT.to_string()),
            binary_commit_sha: None,
        }
    }

    #[test]
    fn test_built_below_worktree_root_flags_subdirectory_and_missing_roots_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let subtree = root.join("crates").join("cli");
        fs::create_dir_all(&subtree).unwrap();
        fs::write(root.join(".git"), "gitdir: elsewhere\n").unwrap();
        let other_root = tmp.path().join("other");
        fs::create_dir_all(other_root.join(".git")).unwrap();

        let git_sha = "0".repeat(40);
        assert!(built_below_worktree_root(&subtree, &git_sha, &root));
        assert!(!built_below_worktree_root(&subtree, &git_sha, &subtree));
        assert!(!built_below_worktree_root(&root, &git_sha, &root));
        assert!(!built_below_worktree_root(&other_root, &git_sha, &root));
        assert!(built_below_worktree_root(
            &tmp.path().join("gone"),
            &git_sha,
            &root
        ));
        assert!(
            !built_below_worktree_root(Path::new("repo"), &git_sha, &root),
            "relative fixtures are unknown"
        );
        assert!(!built_below_worktree_root(Path::new(""), &git_sha, &root));
        let plain = tmp.path().join("plain-source");
        fs::create_dir_all(&plain).unwrap();
        assert!(
            !built_below_worktree_root(&plain, &path_bound_sha(&plain).unwrap(), &root),
            "a non-git source tree is keyed by its own path"
        );
        assert!(built_below_worktree_root(&plain, &git_sha, &root));
    }

    #[test]
    fn test_attach_if_fingerprint_matches_rejects_subtree_slot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let subtree = root.join("sub");
        fs::create_dir_all(&subtree).unwrap();
        fs::write(root.join(".git"), "gitdir: elsewhere\n").unwrap();
        let slot = tmp.path().join("slot");
        fs::create_dir_all(&slot).unwrap();
        let graph = ecp_core::graph_fixture::GraphFixture::new().build();
        fs::write(
            slot.join("graph.bin"),
            rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap(),
        )
        .unwrap();
        CommitBuildMeta::write_atomic(&slot.join("meta.json"), &meta(&subtree.to_string_lossy()))
            .unwrap();
        assert!(attach_if_fingerprint_matches(&slot, &root).is_none());
        CommitBuildMeta::write_atomic(&slot.join("meta.json"), &meta(&root.to_string_lossy()))
            .unwrap();
        assert!(attach_if_fingerprint_matches(&slot, &root).is_some());
    }
}
