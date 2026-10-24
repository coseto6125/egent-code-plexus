//! Auto-ensure index for agent CLI commands.
//!
//! `ensure_index` reports state (Ready / Stale / Missing). `ensure_fresh`
//! is the actionable wrapper:
//!
//! - Missing → warm-attach most-recent sibling SHA + background rebuild (fast
//!   path for OOB branch switch — IDE / external terminal `git checkout` where
//!   no PostToolUse hook fires). Falls back to sync `build_l2` when no sibling
//!   exists.
//! - Stale, header-incompatible → full L2 rebuild (corrupt overlay = corruption risk).
//! - Stale, header-compatible → incremental: `reanalyze_files` for dirty set,
//!   then L1 overlay fragment write under `<repo>/sessions/<sid>/`.
//! - Ready → noop.

use ecp_core::contracts::dirty_event::{
    ClassifiedPath, DirtyEvent, DirtySet, DuplicatePath, RepoPath, SkipReason,
};
use ecp_core::contracts::routing::{FileRouter, SizeCap};
use ecp_core::session::SessionMeta;
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// Call-count trackers for branch-dispatch assertions.
///
/// The module is unconditionally compiled so integration tests can import
/// `ecp_cli::auto_ensure::test_counters` without a feature flag. The counter
/// `fetch_add` sites are always active so integration tests that call
/// `ensure_fresh` directly see the increments. The statics cost two
/// zero-initialised `usize` words in the BSS segment — negligible — and the
/// linker dead-strips them in release builds when nothing reads them.
/// Both counters use `Ordering::Relaxed`; tests assert after a single
/// synchronous `ensure_fresh` call with no concurrent writers.
#[allow(dead_code)]
pub mod test_counters {
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub static REANALYZE_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// The number of paths `ensure_fresh` handed to `reanalyze_files`,
    /// summed over calls: shows that "other" paths never reach the parse.
    pub static REANALYZE_PATH_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static BUILD_L2_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// FU-2026-05-23-047: incremented every time `ensure_fresh` synchronously
    /// joins the background tantivy writer because `~/.ecp/` resolves to a
    /// path inside the current worktree. Observed by the regression test in
    /// `tests/ensure_fresh_tantivy_drain.rs`; in normal prod (cache root
    /// sibling to the repo) this counter stays at 0.
    pub static TANTIVY_JOIN_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// Incremented per parked tantivy writer that `join_pending_tantivy_writers`
    /// waited for at process exit.
    pub static TANTIVY_EXIT_JOIN_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// Incremented every time `ensure_fresh` returns `WarmAttach` instead of
    /// blocking on a cold build. Lets integration tests assert the fast path
    /// fires for OOB branch-switch scenarios.
    pub static WARM_ATTACH_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// Incremented when `load_ensured` detects a behind-HEAD graph and spawns
    /// the background self-heal (the spawn itself is skipped under
    /// ECP_SKIP_BG_REBUILD, so tests assert intent via this counter).
    pub static BEHIND_HEAD_HEAL_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// FU-2026-06-10-f92cecba3daa: incremented when the L1 freshness gate
    /// proves every dirty file's fragment current (manifest mtime_ns match)
    /// and skips the refresh entirely.
    pub static FRESH_GATE_SKIP_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// Incremented each time `spawn_upgrade_gc` passes its once-per-process
    /// guard, before the env / binary-name gates that skip the spawn in
    /// tests. Left out of `reset` with the guard it counts: the guard is
    /// never re-armed, so this reads 1 after any number of drift queries.
    pub static UPGRADE_GC_INTENT_COUNT: AtomicUsize = AtomicUsize::new(0);

    pub fn reset() {
        REANALYZE_CALL_COUNT.store(0, Ordering::Relaxed);
        REANALYZE_PATH_COUNT.store(0, Ordering::Relaxed);
        BUILD_L2_CALL_COUNT.store(0, Ordering::Relaxed);
        TANTIVY_JOIN_COUNT.store(0, Ordering::Relaxed);
        TANTIVY_EXIT_JOIN_COUNT.store(0, Ordering::Relaxed);
        WARM_ATTACH_COUNT.store(0, Ordering::Relaxed);
        BEHIND_HEAD_HEAL_COUNT.store(0, Ordering::Relaxed);
        super::BEHIND_HEAD_SPAWNED.store(false, Ordering::Relaxed);
        FRESH_GATE_SKIP_COUNT.store(0, Ordering::Relaxed);
    }

    pub fn reanalyze_calls() -> usize {
        REANALYZE_CALL_COUNT.load(Ordering::Relaxed)
    }

    pub fn reanalyze_paths() -> usize {
        REANALYZE_PATH_COUNT.load(Ordering::Relaxed)
    }

    pub fn build_l2_calls() -> usize {
        BUILD_L2_CALL_COUNT.load(Ordering::Relaxed)
    }

    pub fn tantivy_join_calls() -> usize {
        TANTIVY_JOIN_COUNT.load(Ordering::Relaxed)
    }

    pub fn warm_attach_calls() -> usize {
        WARM_ATTACH_COUNT.load(Ordering::Relaxed)
    }

    pub fn behind_head_heal_calls() -> usize {
        BEHIND_HEAD_HEAL_COUNT.load(Ordering::Relaxed)
    }

    pub fn fresh_gate_skips() -> usize {
        FRESH_GATE_SKIP_COUNT.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureResult {
    /// Graph exists and is fresher than working tree.
    Ready,
    /// Graph does not exist; caller should index.
    Missing,
    /// Graph exists but working tree has newer files.
    /// `age_seconds` = how long since graph was last built.
    /// `needs_full_rebuild`: true ⇒ the staleness is a convention/fingerprint
    /// drift, not just dirty files — `ensure_fresh` must do a full `build_l2`
    /// (which drops the old graph.bin) rather than an incremental L1 overlay,
    /// because the overlay path leaves stale-convention nodes in place.
    /// `dirty_files`: when the staleness was decided by the git porcelain
    /// shortcut, the classified dirty set it already parsed, "other" events
    /// and ecp's own artifacts dropped — lets `ensure_fresh` skip the
    /// whole-tree mtime walk in `collect_dirty_files` (O(changed) vs O(tree):
    /// ~0.86s → ~0.02s on a 14k-file repo). `None` when the decision came
    /// from the mtime walk (no path list available) or a full-rebuild drift
    /// (dirty set is irrelevant).
    Stale {
        age_seconds: u64,
        needs_full_rebuild: bool,
        dirty_files: Option<DirtySet<ClassifiedPath>>,
    },
}

/// What a caller needs from the index before it can answer honestly.
///
/// This used to be encoded at each call site, and the sites disagreed:
/// `main.rs` served a sibling SHA's graph on `WarmAttach`, while `diff`
/// rejected the identical outcome and forced a foreground rebuild — a
/// difference no user could see, decided by which file the code lived in.
/// Naming the need leaves the choice with the caller and the mechanics here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexNeed<'a> {
    /// A near-current graph answers the question. A sibling SHA that passes
    /// the distance gate may serve this invocation while a rebuild runs in
    /// the background. Every query command wants this.
    NearCurrent,
    /// No sibling commit's graph may stand in, and any build this triggers
    /// targets the given commit (`None` = HEAD). `diff` reads two graphs and
    /// extracts files out of them: a sibling is a *different* commit, and a
    /// background writer would race the read.
    ///
    /// The staleness probe underneath is HEAD-relative — it compares the
    /// sidecar against `git_head_sha` and walks mtimes — so this does **not**
    /// verify that HEAD is already the requested commit. Callers passing
    /// `Some(sha)` must point it at a checkout of that commit, as `diff` does
    /// with its `BaselineCheckout` worktree.
    ExactSha(Option<&'a str>),
}

/// Outcome returned by `ensure_fresh`, disambiguating the warm-attach fast
/// path from a fully synchronous build.
///
/// `IndexNeed::ExactSha` never yields `WarmAttach`: rather than borrowing a
/// sibling commit's graph, it builds in the foreground and returns `Ready`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureFreshOutcome {
    /// Graph is up-to-date (or was synchronously rebuilt / overlaid). A
    /// rebuild publishes a new commit dir, so a caller holding a pre-ensure
    /// path re-resolves it (`graph_path::resolve` for a cwd graph) or loads
    /// through `load_ensured`, which follows the build.
    Ready,
    /// New HEAD has no published graph yet. The most-recent sibling SHA's graph
    /// is usable for this invocation; a background rebuild for the new SHA has
    /// been spawned. The caller should load `sibling_graph_path` and mark the
    /// engine stale so LLM consumers know results may be slightly behind.
    WarmAttach { sibling_graph_path: PathBuf },
}

pub fn ensure_index(graph_path: &Path, worktree_root: &Path) -> io::Result<EnsureResult> {
    probe_index(graph_path, worktree_root).map(|(result, _)| result)
}

type Probe = (EnsureResult, Option<DirtySet<ClassifiedPath>>);

/// [`ensure_index`] plus the dirty set a `Ready` verdict still saw. Its events
/// are empty (nothing changed that a source or config reader needs) but its
/// skipped entries survive, so a caller can still report the paths git printed
/// that the set cannot name. `EnsureResult::Ready` stays a unit variant for the
/// doctor, summary and hook matches that never read the set.
pub(crate) fn probe_index(graph_path: &Path, worktree_root: &Path) -> io::Result<Probe> {
    let graph_mtime = match fs::metadata(graph_path) {
        Ok(m) => m.modified()?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((EnsureResult::Missing, None)),
        Err(e) => return Err(e),
    };

    // A CLI upgrade that bumped GRAPH_FORMAT_VERSION would otherwise surface
    // as engine::Engine::load's InvalidData error on the next query. Treat a
    // schema break the same as a stale graph so ensure_fresh transparently
    // rebuilds.
    if !crate::engine::header_compatible(graph_path) {
        spawn_upgrade_gc();
        return Ok((
            EnsureResult::Stale {
                age_seconds: 0,
                needs_full_rebuild: true,
                dirty_files: None,
            },
            None,
        ));
    }

    // Convention-drift gate (FU-2026-05-25-005). `header_compatible` only
    // catches rkyv layout breaks; an analyzer / path-normalization change that
    // alters which nodes are emitted (e.g. the `src/`-rooted path bug) keeps
    // the format version but changes BUILDER_FINGERPRINT. Without this, the
    // git_fingerprint_shortcut below returns Ready for an unchanged HEAD and
    // the stale nodes survive every reindex until a human runs `--force`.
    // Placed BEFORE the shortcut so a binary upgrade can never be short-
    // circuited past. Stale ⇒ ensure_fresh rebuilds via the header-compatible
    // or build_l2 branch.
    if fingerprint_drifted(graph_path) {
        spawn_upgrade_gc();
        return Ok((
            EnsureResult::Stale {
                age_seconds: 0,
                needs_full_rebuild: true,
                dirty_files: None,
            },
            None,
        ));
    }

    // A slot built from a subdirectory answers `found:false` for everything
    // outside that subtree. Placed before the shortcut for the same reason as
    // the fingerprint gate: the sidecar match would otherwise return Ready for
    // ever. One small JSON read per query buys back the core promise.
    if slot_built_below_root(graph_path, worktree_root) {
        return Ok((
            EnsureResult::Stale {
                age_seconds: 0,
                needs_full_rebuild: true,
                dirty_files: None,
            },
            None,
        ));
    }

    // Fast path: if the working tree is a git repo, the indexed HEAD matches
    // the current HEAD, and `git status --porcelain -uall` is empty, the graph
    // is fresh by construction — skip the 22k-file mtime walk entirely.
    // Saves ~140ms on a typical mid-size repo. None ⇒ fall through to walk.
    if let Some(result) = git_fingerprint_shortcut(graph_path, worktree_root, graph_mtime) {
        return Ok(result);
    }

    if any_source_newer_than(graph_path, worktree_root, graph_mtime)? {
        let age = SystemTime::now()
            .duration_since(graph_mtime)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return Ok((
            EnsureResult::Stale {
                age_seconds: age,
                needs_full_rebuild: false,
                // mtime walk only answers "is anything newer", not "which files";
                // ensure_fresh falls back to collect_dirty_files for the path list.
                dirty_files: None,
            },
            None,
        ));
    }
    Ok((EnsureResult::Ready, None))
}

/// Path of the HEAD-SHA sidecar written next to `graph.bin`. One line, 40 hex
/// chars + LF. Tiny: open+read+parse is a single page of IO.
pub fn head_sha_sidecar_path(graph_path: &Path) -> PathBuf {
    let mut p = graph_path.as_os_str().to_owned();
    p.push(".head_sha");
    PathBuf::from(p)
}

/// Write the current HEAD SHA next to `graph_path`. Resolves HEAD via git
/// when called after the L1 overlay path; build paths that already know
/// the SHA should use `write_head_sha_sidecar_with_sha` to avoid an extra
/// `git rev-parse`.
pub fn write_head_sha_sidecar(graph_path: &Path, worktree_root: &Path) {
    let Ok(sha) = git_head_sha(worktree_root) else {
        return;
    };
    write_head_sha_sidecar_with_sha(graph_path, &sha);
}

/// Same as `write_head_sha_sidecar` but takes an already-known SHA, avoiding
/// a second `git rev-parse`. Synchronous because a short-lived CLI process
/// exits before a detached write lands. A failed write (permissions, full disk) is
/// covered for a graph in a commit dir, whose name `git_fingerprint_shortcut`
/// falls back to. A graph outside one falls back to the mtime walk, which
/// misses edits made before the build.
pub fn write_head_sha_sidecar_with_sha(graph_path: &Path, sha: &str) {
    let _ = fs::write(head_sha_sidecar_path(graph_path), format!("{sha}\n"));
}

/// Path of the format-version sidecar next to `graph.bin`. Single ASCII line
/// holding `GRAPH_FORMAT_VERSION` as decimal. Lets `attach_latest_sibling_sha`
/// skip the 10-50ms `rkyv::access` validation `header_compatible` pays on a
/// 50-200 MB graph.bin (rkyv 0.8 lays the root struct at the file tail, so a
/// fixed-offset magic check is not possible — sidecar is the cheapest route).
pub fn compatible_version_sidecar_path(graph_path: &Path) -> PathBuf {
    let mut p = graph_path.as_os_str().to_owned();
    p.push(".compatible_version");
    PathBuf::from(p)
}

/// Write the current `GRAPH_FORMAT_VERSION` next to `graph_path`. The build
/// writes it inside its staging dir before the rename, so the sidecar is
/// published with `graph.bin` and no process exit can lose it. A reader that
/// finds none (a graph from an older binary) falls back to `header_compatible`.
pub fn write_compatible_version_sidecar(graph_path: &Path) -> io::Result<()> {
    fs::write(
        compatible_version_sidecar_path(graph_path),
        format!("{}\n", ecp_core::graph::GRAPH_FORMAT_VERSION),
    )
}

pub(crate) fn read_compatible_version_sidecar(graph_path: &Path) -> Option<u32> {
    let content = fs::read_to_string(compatible_version_sidecar_path(graph_path)).ok()?;
    content.trim().parse::<u32>().ok()
}

/// Path of the builder-fingerprint sidecar next to `graph.bin`. One ASCII line
/// holding the `BUILDER_FINGERPRINT` (`v<ver>+schema<n>`) that produced this
/// graph. `GRAPH_FORMAT_VERSION` only bumps on rkyv layout breaks; an analyzer
/// or path-normalization change that alters emitted nodes WITHOUT a layout
/// break leaves the format version untouched, so `header_compatible` cannot
/// see it. The fingerprint embeds the crate version and thus moves on every
/// release — `ensure_index` compares it to force a rebuild when the running
/// binary differs from the one that built the cached graph. See
/// FU-2026-05-25-005.
pub fn builder_fingerprint_sidecar_path(graph_path: &Path) -> PathBuf {
    let mut p = graph_path.as_os_str().to_owned();
    p.push(".builder_fingerprint");
    PathBuf::from(p)
}

/// Write `fingerprint`, the builder fingerprint of the build that produced
/// `graph_path`, next to it. The value comes from the build's own data (its
/// `meta.json`), never from the running binary: a back-fill by another binary
/// must not certify a graph it did not build. A stale or missing fingerprint
/// sidecar makes `fingerprint_drifted` fall back to `meta.json`, so the build
/// writes this one inside its staging dir and it is published with
/// `graph.bin`.
pub fn write_builder_fingerprint_sidecar(graph_path: &Path, fingerprint: &str) -> io::Result<()> {
    fs::write(
        builder_fingerprint_sidecar_path(graph_path),
        format!("{fingerprint}\n"),
    )
}

/// True when the cached graph's builder fingerprint differs from the running
/// binary's — meaning analyzer / path-normalization conventions may have
/// changed and the cached nodes are potentially stale. Reads the sidecar
/// first (one page of IO); on a sidecar miss (graph built by a pre-sidecar
/// binary) falls back to the sibling commit's `meta.json`. Returns `false`
/// (no drift) when neither source is readable — a missing fingerprint is
/// treated as "cannot prove drift", deferring to the existing mtime walk so a
/// transient read error never forces a spurious full rebuild.
fn fingerprint_drifted(graph_path: &Path) -> bool {
    // Sidecar fast path: compare the trimmed file contents directly without
    // allocating an owned String — this runs on every ensure_index call.
    if let Ok(raw) = fs::read_to_string(builder_fingerprint_sidecar_path(graph_path)) {
        let stored = raw.trim();
        if !stored.is_empty() {
            return stored != ecp_core::registry::BUILDER_FINGERPRINT;
        }
    }
    // Sidecar miss (graph built by a pre-sidecar binary): fall back to the
    // commit meta.json. A missing fingerprint there too ⇒ "cannot prove drift",
    // so defer to the mtime walk rather than forcing a spurious rebuild.
    match ecp_core::registry::CommitBuildMeta::read(&graph_path.with_file_name("meta.json")) {
        Ok(meta) => meta
            .builder_fingerprint
            .is_some_and(|fp| fp != ecp_core::registry::BUILDER_FINGERPRINT),
        Err(_) => false,
    }
}

pub fn worktree_sidecar_path(graph_path: &Path) -> PathBuf {
    let mut p = graph_path.as_os_str().to_owned();
    p.push(".worktree");
    PathBuf::from(p)
}

/// The slot's sha and recorded worktree root, read like the other sidecars:
/// one line (`<sha> <path>`) next to `graph.bin`. A slot written before the
/// sidecar existed is read from `meta.json` once and the sidecar back-filled,
/// so later queries pay one page of IO rather than a JSON parse.
fn slot_built_below_root(graph_path: &Path, worktree_root: &Path) -> bool {
    let sidecar = worktree_sidecar_path(graph_path);
    // Only the terminator the writer appends is removed; a path may end in
    // any byte. An unreadable or half-written sidecar falls through to
    // meta.json and is rewritten.
    let cached = fs::read_to_string(&sidecar).ok().and_then(|raw| {
        let (sha, built) = raw.strip_suffix('\n')?.split_once(' ')?;
        (sha.len() == 40).then(|| (sha.to_owned(), built.to_owned()))
    });
    let (sha, built) = match cached {
        Some(cached) => cached,
        None => {
            let Ok(meta) =
                ecp_core::registry::CommitBuildMeta::read(&graph_path.with_file_name("meta.json"))
            else {
                return false;
            };
            let _ = fs::write(
                &sidecar,
                format!("{} {}\n", meta.sha, meta.built_from_worktree),
            );
            (meta.sha, meta.built_from_worktree)
        }
    };
    crate::build::orchestrator::built_below_worktree_root(Path::new(&built), &sha, worktree_root)
}

/// Try to decide Ready vs Stale via the cheap git fingerprint.
/// - Returns `Some(Ready)` when HEAD matches the sidecar or the commit dir
///   name AND `git status --porcelain -uall` names no change but "other"
///   paths and ecp's own artifacts.
/// - Returns `Some(Stale)` when HEAD matches but the working tree has
///   uncommitted source or config changes (skip walk; let L1 overlay collect
///   dirty files).
/// - Returns `None` when the fingerprint check is inconclusive (not a git
///   repo, HEAD matches neither the sidecar nor the commit dir, git command
///   failed, or its output is not porcelain v1) so the caller falls back to
///   the full mtime walk.
fn git_fingerprint_shortcut(
    graph_path: &Path,
    worktree_root: &Path,
    graph_mtime: SystemTime,
) -> Option<Probe> {
    // A short-lived process can exit before the detached sidecar write lands,
    // leaving the file missing or empty. The commit dir's name is published
    // with graph.bin, so a graph built at HEAD still takes this path; the
    // mtime walk cannot see edits made before the build.
    let sidecar = fs::read_to_string(head_sha_sidecar_path(graph_path)).unwrap_or_default();
    let dir_sha = graph_path.parent().and_then(commit_dir_sha);
    if sidecar.trim().is_empty() && dir_sha.is_none() {
        return None;
    }
    let head_sha = git_head_sha(worktree_root).ok()?;
    if sidecar.trim() != head_sha && dir_sha.as_deref() != Some(head_sha.as_str()) {
        return None;
    }

    // Flags and their reasons: `STATUS_ARGS` in `git/status.rs`.
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let porcelain = crate::git::status::porcelain_all(worktree_root, &home_ecp).ok()?;
    if !porcelain.status.success() {
        return None;
    }
    if porcelain.stdout.is_empty() {
        return Some((EnsureResult::Ready, None));
    }
    // Output that is not porcelain v1 proves nothing about the tree: take
    // the mtime walk, as for a failed status.
    let dirty = DirtySet::from_porcelain_z(&porcelain.stdout).ok()?;
    let unreadable = dirty
        .skipped()
        .iter()
        .filter(|entry| entry.reason != SkipReason::Directory)
        .count();
    if unreadable > 0 {
        eprintln!(
            "dirty.skipped entries={unreadable} (paths git printed that are not UTF-8 or not repo-relative)"
        );
    }
    // "Other" events leave before any syscall: 12,000 untracked `.csv` files
    // must not send every query down the refresh path (freshness F6).
    let mut dirty = classify_dirty(dirty).without_other();
    if dirty.events().is_empty() {
        return Some((EnsureResult::Ready, Some(dirty)));
    }
    // ecp's own artifacts are filtered out (mirroring `any_source_newer_than`):
    // with `--untracked-files=all` they would otherwise keep porcelain
    // non-empty forever and downgrade every query from the Ready shortcut to
    // the Stale/reanalyze path. A rename keeps its removed half's tombstone,
    // so only a lone worktree half can be an artifact.
    let home_ecp_canonical = fs::canonicalize(&home_ecp).ok();
    let graph_canonical = fs::canonicalize(graph_path).ok();
    dirty.retain(|event| match event {
        DirtyEvent::Added(half) | DirtyEvent::Modified(half) => !is_ecp_artifact(
            &worktree_root.join(half.path.as_str()),
            graph_canonical.as_deref(),
            home_ecp_canonical.as_deref(),
        ),
        DirtyEvent::Deleted(_) | DirtyEvent::Renamed { .. } | DirtyEvent::Unmerged(_) => true,
    });
    if dirty.events().is_empty() {
        return Some((EnsureResult::Ready, Some(dirty)));
    }
    let age = SystemTime::now()
        .duration_since(graph_mtime)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((
        EnsureResult::Stale {
            age_seconds: age,
            needs_full_rebuild: false,
            dirty_files: Some(dirty),
        },
        None,
    ))
}

/// C5 routing, the same table `reanalyze_files` dispatches on. No syscall.
fn classify_dirty(dirty: DirtySet) -> DirtySet<ClassifiedPath> {
    const ROUTER: FileRouter = FileRouter::new(SizeCap::BUILT_IN);
    dirty.classify(&ROUTER, None)
}

/// True when `path` is one of ecp's own files and must not count as dirty
/// source. Two families: anything under the resolved `~/.ecp` cache root
/// (covers ECP_HOME nested inside the worktree, where tantivy segments and
/// session overlays churn on every query), and the graph file plus its
/// `graph.bin.*` sidecars when the graph lives in-tree (legacy layout, test
/// fixtures). Canonicalize failure (e.g. a deleted tracked file) ⇒ treat as
/// a real source change — the conservative direction.
pub(crate) fn is_ecp_artifact(
    path: &Path,
    graph_canonical: Option<&Path>,
    home_ecp_canonical: Option<&Path>,
) -> bool {
    let Ok(p) = fs::canonicalize(path) else {
        return false;
    };
    if home_ecp_canonical.is_some_and(|h| p.starts_with(h)) {
        return true;
    }
    let Some(g) = graph_canonical else {
        return false;
    };
    p.parent() == g.parent()
        && match (
            p.file_name().and_then(|n| n.to_str()),
            g.file_name().and_then(|n| n.to_str()),
        ) {
            (Some(pf), Some(gf)) => pf.starts_with(gf),
            _ => false,
        }
}

/// Ensure the graph exists and is fresher than the working tree.
///
/// - Missing → warm-attach most-recent sibling SHA (background rebuild for new
///   SHA). Falls back to sync `build_l2` when no sibling exists.
/// - Stale, header-incompatible → `build_l2` (full rebuild; applying an L1
///   overlay against an incompatible base schema produces a corrupt graph).
/// - Stale, header-compatible → incremental: `reanalyze_files` for the dirty
///   file set (T7-4), followed by L1 overlay fragment write (T7-5 will replace
///   the overlay write with a zero-copy in-place merge).
/// - Ready → noop.
pub fn ensure_fresh(
    need: IndexNeed<'_>,
    graph_path: &Path,
    worktree_root: &Path,
) -> Result<EnsureFreshOutcome, String> {
    ensure_fresh_with_rebuild(need, graph_path, worktree_root).map(|ensured| ensured.outcome)
}

/// What `ensure_fresh_with_rebuild` did, for the read context.
pub(crate) struct Ensured {
    pub(crate) outcome: EnsureFreshOutcome,
    /// The `graph.bin` a synchronous build published. A rebuild lands in a
    /// new `.gen` commit dir and leaves the probed file in place, so a caller
    /// that loads `graph_path` after a build reads the stale graph (or
    /// nothing, when `graph_path` was the never-built legacy default).
    pub(crate) rebuilt: Option<PathBuf>,
    /// The source and config changes the gate saw; empty when it saw none.
    /// `None` when a build or a warm attach answered and no gate named paths.
    pub(crate) dirty: Option<DirtySet<ClassifiedPath>>,
}

impl Ensured {
    fn built(rebuilt: PathBuf) -> Self {
        Self {
            outcome: EnsureFreshOutcome::Ready,
            rebuilt: Some(rebuilt),
            dirty: None,
        }
    }

    fn current(dirty: Option<DirtySet<ClassifiedPath>>) -> Self {
        Self {
            outcome: EnsureFreshOutcome::Ready,
            rebuilt: None,
            dirty,
        }
    }
}

/// `ensure_fresh` plus the build it published and the dirty set it read.
/// `worktree_root` must be the worktree top: porcelain paths are relative to
/// it (`read_context::worktree_top`).
pub(crate) fn ensure_fresh_with_rebuild(
    need: IndexNeed<'_>,
    graph_path: &Path,
    worktree_root: &Path,
) -> Result<Ensured, String> {
    beat_session_heartbeat(worktree_root);
    let (state, ready_dirty) =
        probe_index(graph_path, worktree_root).map_err(|e| format!("ensure_index probe: {e}"))?;
    // `ExactSha` carries the commit to build; `NearCurrent` builds HEAD.
    let target_sha = match need {
        IndexNeed::ExactSha(sha) => sha,
        IndexNeed::NearCurrent => None,
    };
    match state {
        // Ready proves no source or config change, so the events are empty;
        // the probe's set still names the entries it skipped.
        EnsureResult::Ready => Ok(Ensured::current(
            ready_dirty.or_else(|| DirtySet::from_events(Vec::new()).ok()),
        )),
        EnsureResult::Missing => {
            // A sibling is a different commit, so it is only ever an answer to
            // `NearCurrent`. Deciding here rather than unwinding a warm-attach
            // afterwards keeps `ExactSha` from announcing an attach it does not
            // do and from spawning a background rebuild it does not need.
            if matches!(need, IndexNeed::NearCurrent) {
                if let Some(sibling) = attach_latest_sibling_sha(worktree_root) {
                    let note = spawn_background_rebuild(worktree_root);
                    test_counters::WARM_ATTACH_COUNT
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    eprintln!(
                        "l2.warm-attach sibling={} rebuild=background{note}",
                        sibling.display()
                    );
                    return Ok(Ensured {
                        outcome: EnsureFreshOutcome::WarmAttach {
                            sibling_graph_path: sibling,
                        },
                        rebuilt: None,
                        dirty: None,
                    });
                }
            }
            let start = std::time::Instant::now();
            // build_l2 → build_inside_locked writes the HEAD-SHA sidecar right
            // after its publish; no extra write needed here.
            let mut result = crate::build::orchestrator::build_l2(worktree_root, target_sha)
                .map_err(|e| format!("build_l2: {e}"))?;
            park_tantivy_writer(&mut result, worktree_root);
            eprintln!("l2.built elapsed={:.2}s", start.elapsed().as_secs_f32());
            Ok(Ensured::built(result.commit_dir.join("graph.bin")))
        }
        EnsureResult::Stale {
            needs_full_rebuild,
            dirty_files,
            ..
        } => {
            if needs_full_rebuild || !crate::engine::header_compatible(graph_path) {
                // Full-rebuild staleness: either a version-incompatible base
                // (applying an overlay would silently corrupt graph.bin) or a
                // builder-fingerprint drift (overlay leaves stale-convention
                // nodes in place — FU-2026-05-25-005). Drop + rebuild.
                // Invariant (T1-7 + OQ-5): this branch must NEVER call the overlay path.
                // Counter is incremented before build_l2 so tests can assert branch
                // dispatch even when build_l2 fails in a minimal tempdir fixture.
                test_counters::BUILD_L2_CALL_COUNT
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let start = std::time::Instant::now();
                let mut result = crate::build::orchestrator::build_l2(worktree_root, target_sha)
                    .map_err(|e| format!("build_l2 (incompatible schema): {e}"))?;
                park_tantivy_writer(&mut result, worktree_root);
                eprintln!("l2.rebuilt elapsed={:.2}s", start.elapsed().as_secs_f32());
                Ok(Ensured::built(result.commit_dir.join("graph.bin")))
            } else {
                // Header-compatible + dirty files: incremental refresh path.
                //
                // Prefer the dirty set git already produced in the porcelain
                // shortcut (O(changed)); only fall back to the whole-tree mtime
                // walk when the staleness came from the mtime path itself.
                let dirty = match dirty_files {
                    Some(dirty) => dirty,
                    None => {
                        let graph_mtime = fs::metadata(graph_path)
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::UNIX_EPOCH);
                        let walked = collect_dirty_files(graph_path, worktree_root, graph_mtime)
                            .unwrap_or_default();
                        // Forward slashes throughout: the overlay writer
                        // normalizes manifest keys to `/`, so the gate's
                        // lookups must match on Windows too.
                        let events: Vec<_> = walked
                            .iter()
                            .filter_map(|p| p.strip_prefix(worktree_root).ok()?.to_str())
                            .filter_map(|rel| RepoPath::new(rel.replace('\\', "/")).ok())
                            .map(DirtyEvent::Modified)
                            .collect();
                        let walked =
                            DirtySet::from_events(events).map_err(|DuplicatePath(p)| {
                                format!("mtime walk named {} twice", p.as_str())
                            })?;
                        classify_dirty(walked)
                    }
                };
                // Every source half reaches the gate and the parse, removed
                // halves too, as today's ` D` entries did: `reanalyze_files`
                // stats each path once and skips a missing one. Config-only
                // halves stay in the set for areas 4 and 5.
                let rel_paths: Vec<String> = dirty
                    .events()
                    .iter()
                    .flat_map(|event| event.halves())
                    .filter(|(_, half)| half.class.is_source())
                    .map(|(_, half)| half.path.as_str().to_owned())
                    .collect();
                // Freshness gate (FU-2026-06-10-f92cecba3daa): each fragment
                // records its source file's mtime_ns in `dirty_files.json`, so
                // a dirty file whose mtime still matches was already parsed
                // into a current fragment by an earlier query of this session —
                // re-parsing it rewrites identical state. Filtering those out
                // turns the typical agent burst (30+ queries, no edits in
                // between) from N full L1 refreshes into stat checks.
                let gate = retain_stale_fragments(worktree_root, rel_paths);
                if gate.rel_paths.is_empty() {
                    test_counters::FRESH_GATE_SKIP_COUNT
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Ok(Ensured::current(Some(dirty)));
                }
                // Stat BEFORE parsing: the manifest mtime must belong to the
                // content that was parsed. Statting after the parse would let
                // an edit landing mid-parse record (new mtime, old content) —
                // and the freshness gate would then skip the re-parse that
                // edit needs. A pre-parse mtime in the same race instead
                // records (old mtime, mixed content), which the next query
                // sees as stale — the gate only errs toward re-parsing.
                // The gate already statted every file it compared against the
                // manifest; only files it never statted (no manifest entry,
                // v1 bin, parse_failed) are statted here.
                let mut pre_parse_mtimes = gate.pre_mtimes;
                for rel in &gate.rel_paths {
                    if !pre_parse_mtimes.contains_key(rel) {
                        let ns = mtime_ns_of(&worktree_root.join(rel)).unwrap_or(0);
                        pre_parse_mtimes.insert(rel.clone(), ns);
                    }
                }
                // Parse the dirty set ONCE here; the resulting `LocalGraph`s are
                // handed straight to the overlay writer, which no longer
                // re-parses them. This removes the second full tree-sitter pass
                // (the dominant incremental cost on a large repo: ~0.6s for the
                // 20-provider pipeline init alone).
                let fresh_graphs = crate::reanalyze::reanalyze_files(
                    worktree_root,
                    &crate::git::DiffScope::Unstaged,
                    &gate.rel_paths,
                );
                test_counters::REANALYZE_CALL_COUNT
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                test_counters::REANALYZE_PATH_COUNT
                    .fetch_add(gate.rel_paths.len(), std::sync::atomic::Ordering::Relaxed);

                apply_l1_overlay_updates(
                    worktree_root,
                    &fresh_graphs,
                    &pre_parse_mtimes,
                    gate.session_meta,
                )
                .map_err(|e| format!("L1 overlay refresh: {e}"))?;
                // L1 overlay only touches dirty fragments under
                // `<repo>/sessions/<sid>/` and does not rewrite graph.bin, but the
                // working tree is now consistent with the indexed state of the
                // current HEAD — refresh the fingerprint as the very last step.
                write_head_sha_sidecar(graph_path, worktree_root);
                Ok(Ensured::current(Some(dirty)))
            }
        }
    }
}

/// Version-checked graph load for every path that loads a graph by repo
/// (find multi-repo, group, peers plan) rather than by explicit `--graph`:
/// the engine of [`crate::read_context::ReadContext::for_graph`].
///
/// Runs `ensure_fresh` first so the two-tier staleness contract applies
/// uniformly: a header/fingerprint drift forces a full `build_l2` before the
/// load (else the caller would read nodes a stale binary produced — e.g. a
/// parser bug already fixed upstream), and a dirty working tree triggers the
/// incremental path. On `WarmAttach` (current SHA has no graph yet) it loads
/// the sibling graph and flags it stale so the caller can surface a note.
/// The session overlay is attached, so uncommitted symbols answer here as
/// they do through `main.rs`.
///
/// `--graph <path>` is the one documented opt-out and must NOT route through
/// here: it loads the named graph verbatim via `Engine::load`, since the user
/// is deliberately pointing at a specific graph (e.g. A/B graph comparison).
pub fn load_ensured(
    graph_path: &Path,
    worktree_root: &Path,
) -> Result<crate::engine::Engine, String> {
    crate::read_context::ReadContext::for_graph(graph_path, worktree_root)
        .map(|ctx| ctx.engine)
        .map_err(|e| e.to_string())
}

/// Flag a by-repo graph that predates HEAD and start its rebuild.
///
/// Callers resolve `graph_path` as the latest *published* graph, which lags
/// committed-but-unindexed work. `ensure_fresh` then reports Ready because
/// the L1 overlay absorbed the delta — but the overlay holds only uncommitted
/// files, so the engine must self-flag or a `found:false` would read as a
/// definitive "does not exist".
pub(crate) fn flag_behind_head(
    engine: &mut crate::engine::Engine,
    graph_path: &Path,
    worktree_root: &Path,
) {
    engine.behind_head = graph_behind_head(graph_path, worktree_root);
    // Self-heal: every L1 overlay refresh rewrites the `.head_sha`
    // sidecar to the current HEAD, so `ensure_index` keeps answering
    // Ready and a behind-HEAD L2 never rebuilds on its own (observed:
    // 15 days stale before a manual --force). Reuse the warm-attach
    // spawn — flock-deduped, skipped under ECP_SKIP_BG_REBUILD. The
    // once-per-process guard matters for long-lived MCP processes,
    // which would otherwise fork a probe `sh` on every query for the
    // whole rebuild window; CLI one-shots pay one spawn either way.
    if engine.behind_head && !BEHIND_HEAD_SPAWNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        test_counters::BEHIND_HEAD_HEAL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let note = spawn_background_rebuild(worktree_root);
        eprintln!("l2.behind-head rebuild=background{note}");
    }
}

/// The SHA a published commit dir was built for, parsed out of the dir NAME
/// (`CommitDirName` handles `.gen.<ts>.<pid>.<n>` rebuild suffixes). `None`
/// for legacy layouts or unparseable names.
fn commit_dir_sha(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_str()?;
    ecp_core::registry::CommitDirName::parse(name)
        .ok()
        .map(|p| p.sha_hex())
}

/// True when `graph_path` sits in a commit dir whose SHA differs from the
/// current HEAD of `worktree_root`. The dir NAME is the authority: the
/// `.head_sha` sidecar is rewritten to the current HEAD after every L1
/// overlay refresh, so it cannot distinguish "graph built at HEAD" from
/// "old graph whose overlay was refreshed at HEAD". Inconclusive cases
/// (legacy layout, non-git worktree, unparseable dir name) return `false`
/// — never caveat on a guess.
fn graph_behind_head(graph_path: &Path, worktree_root: &Path) -> bool {
    let Some(graph_sha) = graph_path.parent().and_then(commit_dir_sha) else {
        return false;
    };
    let Some(head) = crate::git_cache::head_sha(worktree_root) else {
        return false;
    };
    graph_sha != head
}

/// Process-lifetime cache for `attach_latest_sibling_sha` results, keyed by
/// `worktree_root`. CLI processes call once per invocation so the cache is a
/// no-op; long-lived MCP processes repeating ensure_fresh while a rebuild is
/// in flight see all subsequent calls short-circuit. Once the rebuild lands,
/// the new SHA's graph short-circuits `ensure_fresh` itself before reaching
/// this path, so cache staleness across rebuilds is harmless.
static SIBLING_CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<PathBuf>>>> = OnceLock::new();

/// Once-per-process gate for the behind-HEAD self-heal spawn. Not reset when
/// the heal lands: after a successful rebuild `graph_behind_head` goes false,
/// and a repo that falls behind AGAIN within one MCP process lifetime keeps
/// the caveat but waits for the next process to re-spawn — an accepted
/// trade-off against re-introducing a per-query fork.
static BEHIND_HEAD_SPAWNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

static UPGRADE_GC_SPAWNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Returns the `graph.bin` path of the best published sibling commit dir for
/// this repo to warm-attach, or `None` if none qualifies.
///
/// Scans `~/.ecp/<repo>/commits/` newest-first by `graph.bin` mtime and returns
/// the first dir that is both header-compatible AND within
/// `WARM_ATTACH_MAX_DISTANCE` commits of HEAD. Mtime ordering (rather than
/// generation order) is intentional: the most recently *written* graph is the
/// freshest warm base regardless of which branch produced it. Iterating rather
/// than taking only the single newest matters because the newest sibling may be
/// a parallel-branch build that fails the distance gate — giving up there would
/// leave a usable distance-1 sibling unused and force an unnecessary sync build.
fn attach_latest_sibling_sha(worktree_root: &Path) -> Option<PathBuf> {
    let cache = SIBLING_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some(cached) = map.get(worktree_root) {
            return cached.clone();
        }
    }
    let result = attach_latest_sibling_sha_uncached(worktree_root);
    if result.is_some() {
        if let Ok(mut map) = cache.lock() {
            map.insert(worktree_root.to_path_buf(), result.clone());
        }
    }
    result
}

/// Cap on sibling dirs probed per warm-attach. A within-distance sibling
/// (distance ≤ `WARM_ATTACH_MAX_DISTANCE`) is, by definition, HEAD or its direct
/// parent — overwhelmingly among the few most-recently-built graphs. Bounding
/// the scan keeps the worst case (a repo with many parallel-branch builds) to a
/// handful of `git rev-list` execs instead of one per published commit, while
/// the common case (newest sibling is the parent) still returns on the first.
const WARM_ATTACH_PROBE_LIMIT: usize = 8;

fn attach_latest_sibling_sha_uncached(worktree_root: &Path) -> Option<PathBuf> {
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let repo_dir_name = crate::repo_identity::repo_dir_name_for_cwd(worktree_root).ok()?;
    let commits_dir = home_ecp.join(&repo_dir_name).join("commits");
    // Newest-first, so the first qualifying sibling is also the freshest one.
    for sibling_dir in crate::commit_lookup::find_all_by_mtime_desc(&commits_dir)
        .into_iter()
        .take(WARM_ATTACH_PROBE_LIMIT)
    {
        let graph_bin = sibling_dir.join("graph.bin");
        if !graph_bin.is_file()
            || !sibling_graph_compatible(&graph_bin)
            || slot_built_below_root(&graph_bin, worktree_root)
        {
            continue;
        }
        // Staleness gate: warm-attach trades a ~0.3s sync build for a possibly-stale
        // graph. That trade only pays when the sibling is near HEAD — sync cost is
        // ~constant in commit distance, but symbols a distant sibling silently drops
        // grow with it (≈13 at distance 1, ≈252 at distance 44), making `ecp find`
        // answer `found:false` for code that genuinely landed. Too far behind ⇒
        // skip this candidate and try the next-newest; if none qualifies, the
        // caller falls back to a sync build of the correct graph.
        if !sibling_within_warm_distance(&sibling_dir, worktree_root) {
            continue;
        }
        return Some(graph_bin);
    }
    None
}

/// Max commit distance a sibling may sit behind HEAD and still be warm-attached.
/// 1 = HEAD's direct parent only (the "just committed, not yet reindexed" case).
/// See the staleness-gate comment in `attach_latest_sibling_sha_uncached`.
const WARM_ATTACH_MAX_DISTANCE: usize = 1;

/// True when `sibling_dir` names a commit that is an ancestor of the current
/// HEAD at most `WARM_ATTACH_MAX_DISTANCE` commits back. A non-ancestor
/// sibling (parallel branch — its diff vs HEAD is unbounded and uncontrolled)
/// or one whose SHA can't be resolved is treated as "too far": the conservative
/// choice is a sync build, never a stale warm graph.
///
/// `None`-defaulting on any git failure (not a repo, detached state, sibling
/// SHA absent from the object DB after history rewrite) deliberately refuses
/// the warm path — a missed warm-attach costs one sync build, a wrongly-granted
/// one serves silently stale results.
fn sibling_within_warm_distance(sibling_dir: &Path, worktree_root: &Path) -> bool {
    let Some(sibling_sha) = commit_dir_sha(sibling_dir) else {
        return false;
    };
    let Ok(head_sha) = git_head_sha(worktree_root) else {
        return false;
    };
    if sibling_sha == head_sha {
        return true;
    }
    // `--count <sibling>..HEAD` = number of commits reachable from HEAD but not
    // from the sibling. For a sibling that is a direct ancestor this is exactly
    // the commit distance; for a parallel-branch sibling it counts HEAD-side
    // commits since the merge-base, which is also a valid "too far" signal.
    let range = format!("{sibling_sha}..HEAD");
    let output = crate::git::safe_exec::git()
        .args(["rev-list", "--count", &range])
        .current_dir(worktree_root)
        .output();
    let Ok(out) = output else { return false };
    if !out.status.success() {
        return false;
    }
    let Ok(count) = String::from_utf8_lossy(&out.stdout).trim().parse::<usize>() else {
        return false;
    };
    count <= WARM_ATTACH_MAX_DISTANCE
}

/// Cheap compatibility predicate for `attach_latest_sibling_sha`: reads the
/// 4-byte version sidecar written by the build orchestrator. Falls back to
/// `engine::header_compatible` (mmap + rkyv::access, 10-50ms cold) when the
/// sidecar is absent, so legacy `~/.ecp/` caches built before this landed
/// still warm-attach correctly.
fn sibling_graph_compatible(graph_path: &Path) -> bool {
    if let Some(v) = read_compatible_version_sidecar(graph_path) {
        v == ecp_core::graph::GRAPH_FORMAT_VERSION
    } else {
        crate::engine::header_compatible(graph_path)
    }
}

/// Fire-and-forget background rebuild for `worktree_root`. Returns the
/// suffix for the stderr line announcing it: ` last=failed log=<path>` while
/// the previous background rebuild's failure stands, else empty.
///
/// Uses the same `spawn_bg` + `flock -n` pattern as `post_tool_use`'s
/// `spawn_background_reindex`. Concurrent triggers no-op because `flock -n`
/// exits immediately if another builder holds the lock.
fn spawn_background_rebuild(worktree_root: &Path) -> String {
    // Integration tests exercise the warm-attach outcome + counter but have no
    // use for the detached rebuild; skipping it avoids a leaked `sh` subprocess
    // that nextest flags as LEAK on Windows (the rebuild outlives the test).
    if std::env::var_os("ECP_SKIP_BG_REBUILD").is_some() {
        return String::new();
    }
    let Ok(repo_dir_name) = crate::repo_identity::repo_dir_name_for_cwd(worktree_root) else {
        return String::new();
    };
    let markers =
        RebuildMarkers::under(&ecp_core::registry::resolve_home_ecp().join(repo_dir_name));
    spawn_background_rebuild_with(worktree_root, &markers, crate::background::spawn_bg);
    markers.last_failed_note()
}

/// The background rebuild's lock and outcome files, all under
/// `~/.ecp/<repo>/`: a query path writes nothing into the user's worktree.
struct RebuildMarkers {
    lock: PathBuf,
    log: PathBuf,
    complete: PathBuf,
    failed: PathBuf,
}

impl RebuildMarkers {
    fn under(repo_root: &Path) -> Self {
        Self {
            lock: repo_root.join(".warm-attach-rebuild.lock"),
            log: repo_root.join(".warm-attach-rebuild.log"),
            complete: repo_root.join(".warm-attach-rebuild-complete"),
            failed: repo_root.join(".warm-attach-rebuild-failed"),
        }
    }

    /// Without this, a rebuild that never lands leaves every later query on
    /// the warm-attached or behind-HEAD graph with no sign of why.
    fn last_failed_note(&self) -> String {
        if self.failed.exists() {
            format!(" last=failed log={}", self.log.display())
        } else {
            String::new()
        }
    }

    /// The job writes the failed marker itself, so a launcher that never
    /// started must write it here.
    fn record_launch_failure(&self) {
        let _ = std::fs::remove_file(&self.complete);
        let _ = std::fs::write(&self.failed, b"");
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
        {
            let _ = writeln!(log, "rebuild launcher did not start");
        }
    }
}

fn spawn_background_rebuild_with(
    worktree_root: &Path,
    markers: &RebuildMarkers,
    spawn: impl FnOnce(crate::background::BgJob<'_>) -> bool,
) {
    let repo_str = worktree_root.to_string_lossy();
    let args = ["admin", "index", "--repo", repo_str.as_ref()];
    let started = spawn(crate::background::BgJob {
        args: &args,
        lock: &markers.lock,
        cwd: worktree_root,
        retry: (1, 0),
        markers: Some(crate::background::BgMarkers {
            log: &markers.log,
            complete: &markers.complete,
            failed: &markers.failed,
        }),
        then_args: None,
    });
    if !started {
        markers.record_launch_failure();
    }
}

/// A binary upgrade (format or fingerprint change) is when the old caches
/// become garbage, and the first query after it lands here on every host,
/// whichever package manager installed the binary. Shares SessionStart's
/// `.prune.lock`, so the two triggers never run gc concurrently; `admin gc`
/// keeps a grace period for caches another installed version may still use.
///
/// The drift branch answers every query until a rebuild lands, and callers
/// such as `summary` and `doctor` never rebuild. Once per process stops a
/// long-lived MCP process forking `sh` per query; `--auto` stops each
/// one-shot CLI call running a full gc. The `--auto` stamp names the binary
/// that wrote it, so this binary's first trigger still runs once.
fn spawn_upgrade_gc() {
    if UPGRADE_GC_SPAWNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    test_counters::UPGRADE_GC_INTENT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if std::env::var_os("ECP_SKIP_BG_REBUILD").is_some() {
        return;
    }
    // In-process tests run a test harness as `current_exe`; only the real
    // binary may re-exec itself as `admin gc`.
    let is_ecp = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|stem| stem == "ecp"))
        .unwrap_or(false);
    if !is_ecp {
        return;
    }
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let lock = home_ecp.join(".prune.lock");
    let _ = crate::background::spawn_bg(crate::background::BgJob {
        args: &["admin", "gc", "--auto"],
        lock: &lock,
        cwd: &home_ecp,
        retry: (1, 0),
        markers: None,
        then_args: None,
    });
}

/// FU-2026-05-23-047: when `resolve_home_ecp()` resolves to a path NESTED
/// under the current worktree, the background tantivy writer spawned by
/// `build_l2` (orchestrator.rs:233) will race with any subsequent
/// `git stash push -u` or `git status` — `git` enumerates and removes
/// untracked directories, but the writer keeps appending `.tmpXXX` segment
/// files into `tantivy/`, producing "Directory not empty" / "No such file".
///
/// In normal production layouts (`~/.ecp/` is sibling to the repo, not
/// inside it), the prefix check is false and we keep the perf optimisation
/// of returning before the tantivy thread finishes. The only callers that
/// trip this are test fixtures that overload `HOME=$REPO` and the unusual
/// `ECP_HOME=$REPO/.cache` setup.
fn drain_tantivy_if_inside_worktree(
    result: &mut crate::build::orchestrator::BuildResult,
    worktree_root: &Path,
) -> bool {
    let cache_root = ecp_core::registry::resolve_home_ecp();
    if cache_root.starts_with(worktree_root) {
        result.join_background();
        test_counters::TANTIVY_JOIN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return true;
    }
    false
}

/// Writers still running when the query finished. A `JoinHandle` dropped
/// with the `BuildResult` detached the thread, and a CLI process exits the
/// moment its answer is printed: the writer died between `create_in_dir`
/// (which writes `meta.json` with `segments: []`) and `commit()`, leaving an
/// index that opens cleanly and matches nothing, for every later query on
/// that commit. `main` joins these after the answer is out.
static PENDING_TANTIVY_WRITERS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Keep the query fast (the writer keeps running in the background) but keep
/// the handle, so the process cannot exit underneath the writer.
fn park_tantivy_writer(result: &mut crate::build::orchestrator::BuildResult, worktree_root: &Path) {
    if drain_tantivy_if_inside_worktree(result, worktree_root) {
        return;
    }
    if let Some(handle) = result.tantivy_handle.take() {
        if let Ok(mut pending) = PENDING_TANTIVY_WRITERS.lock() {
            // A long-lived MCP process parks one writer per build; drop the
            // ones already done so the list stays a handful, not a history.
            pending.retain(|h| !h.is_finished());
            pending.push(handle);
        }
    }
}

/// Wait for every parked tantivy writer. Called once, after the command has
/// written its output, so the join costs the user nothing they can see.
pub fn join_pending_tantivy_writers() {
    let handles: Vec<_> = PENDING_TANTIVY_WRITERS
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or_default();
    for handle in handles {
        test_counters::TANTIVY_EXIT_JOIN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Err(panic) = handle.join() {
            tracing::warn!("tantivy background thread panicked: {panic:?}");
        }
    }
}

/// Resolve the L1 session overlay dir for `worktree_root` (the worktree top,
/// which keys the fragments' paths), matching exactly the
/// path `apply_l1_overlay_updates` writes to: `<home_ecp>/<repo_dir>/sessions/
/// <session_id>`. Returns `None` when the repo identity can't be resolved. The
/// dir may not exist yet (no overlay written this session) — the caller checks.
pub fn resolve_session_overlay_dir(worktree_root: &Path) -> Option<PathBuf> {
    let session_id = crate::session::resolver::resolve_session_id(None);
    ecp_core::registry::validate_cache_component(&session_id).ok()?;
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let repo_dir = crate::repo_identity::repo_dir_name_for_cwd(worktree_root).ok()?;
    Some(home_ecp.join(&repo_dir).join("sessions").join(&session_id))
}

/// Stat a file's mtime as nanoseconds since the epoch. The shared form of a
/// chain that otherwise gets re-derived at every call site (gate, pre-parse
/// stat, overlay reader).
pub(crate) fn mtime_ns_of(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
}

/// What the L1 freshness gate hands back to `ensure_fresh`: the dirty files
/// still needing a re-parse, plus the work it already did that the miss path
/// would otherwise redo — pre-parse mtimes for the files it statted (the
/// stat-before-parse TOCTOU contract holds: the gate runs before any parse)
/// and the session meta it loaded (so `apply_l1_overlay_updates` doesn't read
/// `session_meta.json` a second time).
struct L1FreshnessGate {
    rel_paths: Vec<String>,
    pre_mtimes: HashMap<String, u64>,
    session_meta: Option<SessionMeta>,
}

/// L1 freshness gate: drop dirty files whose overlay fragment is provably
/// current, keep the rest for re-parse.
///
/// A fragment is current when its `dirty_files.json` entry is a v5 bin that
/// parsed cleanly, is on disk, AND records exactly the file's present
/// mtime_ns, in a manifest the running binary's parser wrote. Any doubt (no
/// session dir / meta / manifest, HEAD drifted since the fragments were
/// based, another builder fingerprint, older bin, parse_failed, missing bin,
/// stat error) keeps the file in the re-parse set — the gate may only ever
/// skip work it can prove redundant.
///
/// The HEAD-drift check matters: promotion (rebasing fragments onto the new
/// base SHA) runs inside the full overlay path, so a drifted session must
/// fall through to it rather than short-circuit here.
fn retain_stale_fragments(worktree_root: &Path, rel_paths: Vec<String>) -> L1FreshnessGate {
    use ecp_core::session::{DirtyFiles, FRAGMENT_FORMAT_V5};

    let pass = |rel_paths, session_meta| L1FreshnessGate {
        rel_paths,
        pre_mtimes: HashMap::new(),
        session_meta,
    };
    let Some(session_dir) = resolve_session_overlay_dir(worktree_root) else {
        return pass(rel_paths, None);
    };
    let Ok(meta) = SessionMeta::read(&session_dir.join("session_meta.json")) else {
        return pass(rel_paths, None);
    };
    let Ok(head) = git_head_sha(worktree_root) else {
        return pass(rel_paths, Some(meta));
    };
    if meta.base_sha != head {
        return pass(rel_paths, Some(meta));
    }
    let Ok(manifest) = DirtyFiles::read(&session_dir.join("dirty_files.json")) else {
        return pass(rel_paths, Some(meta));
    };
    // Fragments another binary parsed hold other parse rules
    // (FU-2026-10-05-36d0314fefe4): every dirty file is re-parsed.
    if !manifest.from_current_builder() {
        return pass(rel_paths, Some(meta));
    }
    let overlay_dir = session_dir.join("graph_overlay");
    let mut pre_mtimes = HashMap::new();
    let retained = rel_paths
        .into_iter()
        .filter(|rel| {
            let Some(entry) = manifest.entries.get(rel) else {
                return true;
            };
            if entry.format != FRAGMENT_FORMAT_V5 || entry.parse_failed {
                return true;
            }
            // A bin the manifest names can still be gone (a cleanup that
            // raced its writer, a hand-deleted file); the reader skips a
            // missing bin, so without the re-parse the file serves L2 until
            // it changes again.
            match mtime_ns_of(&worktree_root.join(rel)) {
                Some(ns)
                    if ns == entry.mtime_ns
                        && fs::read(overlay_dir.join(format!("{}.bin", entry.fragment_id)))
                            .ok()
                            .is_some_and(|bytes| {
                                crate::session::overlay_reader::valid_v5_fragment(&bytes)
                            }) =>
                {
                    false
                }
                Some(ns) => {
                    pre_mtimes.insert(rel.clone(), ns);
                    true
                }
                None => true,
            }
        })
        .collect();
    L1FreshnessGate {
        rel_paths: retained,
        pre_mtimes,
        session_meta: Some(meta),
    }
}

fn apply_l1_overlay_updates(
    worktree_root: &Path,
    fresh_graphs: &[ecp_core::analyzer::types::LocalGraph],
    pre_parse_mtimes: &HashMap<String, u64>,
    gate_meta: Option<SessionMeta>,
) -> io::Result<()> {
    use crate::session::{overlay_writer, promotion};

    // Same path the reader (`resolve_session_overlay_dir`) consults — kept in one
    // place so writer and reader can never look at different session dirs.
    let session_dir = resolve_session_overlay_dir(worktree_root)
        .ok_or_else(|| io::Error::other("cannot resolve repo identity for session overlay dir"))?;
    fs::create_dir_all(&session_dir)?;
    ensure_session_meta(&session_dir, worktree_root)?;

    // ── HEAD-drift promotion check ────────────────────────────────────────
    let current_head = git_head_sha(worktree_root)?;
    let sm_path = session_dir.join("session_meta.json");
    // The freshness gate read the meta moments ago — reuse it; read only when
    // the gate disengaged before loading it (no session dir / first query,
    // where `ensure_session_meta` above may have just created the file).
    let session_meta = match gate_meta {
        Some(m) => m,
        None => SessionMeta::read(&sm_path)?,
    };
    if session_meta.base_sha != current_head {
        match promotion::promotion_case(&session_meta.base_sha, &current_head, worktree_root) {
            promotion::PromotionCase::A => {
                let stats = promotion::promote_case_a(&session_dir, worktree_root, &current_head)?;
                eprintln!(
                    "session.promoted case=A dropped={} kept={}",
                    stats.dropped, stats.kept
                );
            }
            promotion::PromotionCase::B => {
                promotion::promote_case_b(&session_dir, &session_meta.base_sha, &current_head)?;
                eprintln!("session.rebased case=B");
            }
        }
    }
    // ─────────────────────────────────────────────────────────────────────

    // `fresh_graphs` were already parsed by `reanalyze_files`; the overlay
    // writer consumes them directly (no second tree-sitter pass). Each file's
    // mtime was statted by the caller BEFORE the parse, so the manifest entry
    // can only under-claim freshness, never over-claim it (see the freshness
    // gate in `ensure_fresh`). A missing entry records 0 — no real mtime
    // matches it, so that fragment is permanently treated as stale.
    let mtimes: Vec<u64> = fresh_graphs
        .iter()
        .map(|g| {
            pre_parse_mtimes
                .get(g.file_path.to_string_lossy().replace('\\', "/").as_str())
                .copied()
                .unwrap_or(0)
        })
        .collect();

    let (n_written, n_failed) = match overlay_writer::write_dirty_fragments_from_graphs(
        &session_dir,
        fresh_graphs,
        &mtimes,
    ) {
        Ok(outs) => {
            let n_failed = outs.iter().filter(|o| o.parse_failed).count();
            (outs.len() - n_failed, n_failed)
        }
        Err(e) => {
            eprintln!("warning: overlay batch write: {e}");
            (0, 0)
        }
    };
    if n_written > 0 || n_failed > 0 {
        eprintln!("l1.refreshed written={n_written} failed={n_failed}");
    }
    Ok(())
}

/// Smallest gap between two `last_touched` rewrites. Two orders of magnitude
/// under `SESSION_LIVE_TTL_MINS`, so a session stays continuously live while
/// an agent works and the query path pays one extra write per minute at most.
const SESSION_HEARTBEAT_SECS: u64 = 60;

/// Record that this session is still being worked in.
///
/// The pid in `session_meta.json` belongs to the process writing it, which
/// exits in milliseconds, so `last_touched` is the only evidence a peer can
/// read to tell a working agent from an abandoned overlay
/// (FU-2026-06-10-cc120f78889c). Every query beats, not just the ones that
/// re-parse a file: an agent that edits and then spends half an hour reading
/// still holds those files dirty, and must not vanish from `peers status`
/// while it does.
///
/// A session that was never enrolled stays un-enrolled — beating would mint a
/// session dir for every read-only query and hand peers a crowd of sessions
/// with nothing dirty in them. The `mtime` throttle keeps the cost at one
/// read+write per minute; every other invocation pays a single stat.
pub(crate) fn beat_session_heartbeat(worktree_root: &Path) {
    if let Some(dir) = resolve_session_overlay_dir(worktree_root) {
        beat_meta_heartbeat(&dir.join("session_meta.json"));
    }
}

fn beat_meta_heartbeat(meta_path: &Path) {
    let fresh = fs::metadata(meta_path)
        .and_then(|s| s.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age.as_secs() < SESSION_HEARTBEAT_SECS);
    if fresh {
        return;
    }
    if let Ok(mut meta) = SessionMeta::read(meta_path) {
        meta.last_touched = chrono::Utc::now().to_rfc3339();
        let _ = SessionMeta::write_atomic(meta_path, &meta);
    }
}

fn ensure_session_meta(session_dir: &Path, worktree: &Path) -> io::Result<()> {
    let meta_path = session_dir.join("session_meta.json");
    if meta_path.exists() {
        // Team harnesses may export ECP_AGENT_NAME / CLAUDE_AGENT_NAME after
        // this session's meta was first written — refresh so peers see the
        // name. The env check gates the extra read: solo sessions (no env)
        // keep the stat-only fast path. Env can't change within a process,
        // so one refresh per session dir suffices — without this, every
        // Stale-arm query in team mode pays a meta read+parse.
        static REFRESHED_DIRS: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> =
            OnceLock::new();
        let first_visit = REFRESHED_DIRS
            .get_or_init(|| Mutex::new(std::collections::HashSet::new()))
            .lock()
            .map(|mut s| s.insert(session_dir.to_path_buf()))
            .unwrap_or(true);
        if first_visit {
            if let Some(name) = crate::session::resolver::resolve_agent_name() {
                if let Ok(mut meta) = SessionMeta::read(&meta_path) {
                    if meta.apply_agent_name(Some(name)) {
                        SessionMeta::write_atomic(&meta_path, &meta)?;
                    }
                }
            }
        }
        return Ok(());
    }
    let head_sha = git_head_sha(worktree)?;
    let sid = session_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let sm = SessionMeta {
        version: 1,
        session_id: sid,
        pid: Some(std::process::id()),
        started_at: now.clone(),
        last_touched: now,
        base_sha: head_sha,
        source_worktree: worktree.to_string_lossy().into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: crate::session::resolver::resolve_agent_name(),
    };
    SessionMeta::write_atomic(&meta_path, &sm)
}

fn git_head_sha(worktree: &Path) -> io::Result<String> {
    crate::git_cache::head_sha(worktree)
        .ok_or_else(|| io::Error::other("git rev-parse HEAD failed"))
}

/// ecp-owned cache dirs that callers can't be expected to list in their
/// own `.gitignore`. Everything else — `target/`, `node_modules/`,
/// `__pycache__/`, `dist/`, `.venv/`, etc — is honoured via `.git_ignore(true)`
/// plus `.ecpignore` on the `WalkBuilder`, so project ignore files are the
/// source of truth for what counts as a "source change". `.git` stays here as a
/// belt-and-suspenders guard: `WalkBuilder` already filters it, but
/// `filter_entry` runs before that, and a project with no `.gitignore` at
/// all would otherwise walk `.git/` mtimes (noisy as hell).
const SKIP_DIRS: &[&str] = &[".git", ".ecp"];

/// Short-circuits on the first source file newer than `graph_mtime`.
/// Walking the full tree only happens when the graph is genuinely
/// fresh — Stale repos exit early at first hit.
fn any_source_newer_than(
    graph_path: &Path,
    root: &Path,
    graph_mtime: SystemTime,
) -> io::Result<bool> {
    let graph_canonical = fs::canonicalize(graph_path).ok();
    let sidecar_canonical = fs::canonicalize(head_sha_sidecar_path(graph_path)).ok();

    let filter_root = root.to_path_buf();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .add_custom_ignore_filename(crate::ECP_IGNORE_FILE)
        .require_git(false)
        .filter_entry(move |e| {
            let name = e.file_name().to_string_lossy();
            if SKIP_DIRS.contains(&name.as_ref()) {
                return false;
            }
            !crate::walker_filter::is_skippable_worktree_descendant(e.path(), &filter_root)
        })
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
    {
        let entry_canonical = fs::canonicalize(entry.path()).ok();
        let skip = entry_canonical.as_deref().is_some_and(|entry| {
            graph_canonical.as_deref() == Some(entry) || sidecar_canonical.as_deref() == Some(entry)
        });
        if skip {
            continue;
        }

        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                if mtime > graph_mtime {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Returns all source files under `root` with mtime newer than `graph_mtime`.
/// Same walk logic as `any_source_newer_than` but collects instead of
/// short-circuiting — used by `apply_l1_overlay_updates`.
fn collect_dirty_files(
    graph_path: &Path,
    root: &Path,
    graph_mtime: SystemTime,
) -> io::Result<Vec<PathBuf>> {
    let graph_canonical = fs::canonicalize(graph_path).ok();
    let mut out = Vec::new();

    let filter_root = root.to_path_buf();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .add_custom_ignore_filename(crate::ECP_IGNORE_FILE)
        .require_git(false)
        .filter_entry(move |e| {
            let name = e.file_name().to_string_lossy();
            if SKIP_DIRS.contains(&name.as_ref()) {
                return false;
            }
            !crate::walker_filter::is_skippable_worktree_descendant(e.path(), &filter_root)
        })
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
    {
        let skip = graph_canonical
            .as_deref()
            .is_some_and(|gc| fs::canonicalize(entry.path()).ok().as_deref() == Some(gc));
        if skip {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                if mtime > graph_mtime {
                    out.push(entry.path().to_path_buf());
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod fingerprint_drift_tests {
    use super::*;
    use tempfile::tempdir;

    fn gate_with_fragment(
        fragment: &[u8],
        session_id: &str,
        format: u32,
        source: &str,
    ) -> (tempfile::TempDir, L1FreshnessGate) {
        use ecp_core::session::{DirtyEntry, DirtyFiles, SessionMeta};

        let tmp = tempdir().unwrap();
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&worktree)
                .status()
                .unwrap()
                .success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "test"]);
        fs::write(worktree.join("app.py"), source).unwrap();
        git(&["add", "app.py"]);
        git(&["commit", "-qm", "base"]);

        let home = tmp.path().join("ecp-home");
        std::env::set_var("ECP_HOME", &home);
        std::env::set_var("ECP_SESSION_ID", session_id);
        let session_dir = resolve_session_overlay_dir(&worktree).unwrap();
        fs::create_dir_all(session_dir.join("graph_overlay")).unwrap();
        let mtime = mtime_ns_of(&worktree.join("app.py")).unwrap();
        let meta = SessionMeta {
            version: 1,
            session_id: session_id.into(),
            pid: None,
            started_at: String::new(),
            last_touched: String::new(),
            base_sha: git_head_sha(&worktree).unwrap(),
            source_worktree: worktree.to_string_lossy().into(),
            overlay_version: 1,
            watcher_pid: None,
            last_drained_offset: 0,
            agent_name: None,
        };
        SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &meta).unwrap();
        let mut dirty = DirtyFiles::empty();
        dirty.entries.insert(
            "app.py".into(),
            DirtyEntry {
                mtime_ns: mtime,
                content_hash: "fixture".into(),
                fragment_id: "fixture".into(),
                tantivy_delta_segment: None,
                parse_failed: false,
                dirty_symbols: vec![],
                format,
            },
        );
        DirtyFiles::write_atomic(&session_dir.join("dirty_files.json"), &dirty).unwrap();
        fs::write(session_dir.join("graph_overlay/fixture.bin"), fragment).unwrap();

        let gate = retain_stale_fragments(&worktree, vec!["app.py".into()]);
        (tmp, gate)
    }

    #[test]
    fn test_retain_stale_fragments_current_v5_skips_reparse() {
        const CHILD_ENV: &str = "ECP_AUTO_ENSURE_V5_REFRESH_CHILD";
        const TEST_NAME: &str =
            "auto_ensure::fingerprint_drift_tests::test_retain_stale_fragments_current_v5_skips_reparse";

        if std::env::var_os(CHILD_ENV).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env(CHILD_ENV, "1")
                .status()
                .unwrap();
            assert!(status.success(), "isolated test process failed: {status}");
            return;
        }

        let fragment = crate::session::overlay_writer::FragmentFile {
            symbols: vec![],
            imports: vec![],
            export_aliases: vec![],
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&fragment).unwrap();
        let (tmp, gate) = gate_with_fragment(
            &bytes,
            "v5-refresh-test",
            ecp_core::session::FRAGMENT_FORMAT_V5,
            "def app(): pass\n",
        );
        assert!(
            gate.rel_paths.is_empty(),
            "a current v5 fragment was re-parsed"
        );
        let worktree = tmp.path().join("repo");
        let session_dir = resolve_session_overlay_dir(&worktree).unwrap();
        let path = session_dir.join("dirty_files.json");
        let mut dirty = ecp_core::session::DirtyFiles::read(&path).unwrap();
        dirty.builder_fingerprint = Some("v0.16.2+schema19".into());
        assert!(!dirty.from_current_builder());
        ecp_core::session::DirtyFiles::write_atomic(&path, &dirty).unwrap();
        assert_eq!(
            retain_stale_fragments(&worktree, vec!["app.py".into()]).rel_paths,
            ["app.py"],
            "a valid, unchanged V5 fragment from another builder must be reparsed"
        );
    }

    #[test]
    fn test_retain_stale_fragments_corrupt_v5_reparses() {
        const CHILD_ENV: &str = "ECP_AUTO_ENSURE_V5_CORRUPT_CHILD";
        const TEST_NAME: &str =
            "auto_ensure::fingerprint_drift_tests::test_retain_stale_fragments_corrupt_v5_reparses";

        if std::env::var_os(CHILD_ENV).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env(CHILD_ENV, "1")
                .status()
                .unwrap();
            assert!(status.success(), "isolated test process failed: {status}");
            return;
        }

        for corrupt in [b"".as_slice(), b"not a fragment".as_slice()] {
            let (_tmp, gate) = gate_with_fragment(
                corrupt,
                "v5-corrupt-test",
                ecp_core::session::FRAGMENT_FORMAT_V5,
                "def app(): pass\n",
            );
            assert_eq!(gate.rel_paths, ["app.py"]);
        }
    }

    #[test]
    fn test_retain_stale_fragments_native_v4_refresh_restores_call_edge() {
        use ecp_analyzer::python::PythonProvider;
        use ecp_analyzer::resolution::builder::GraphBuilder;
        use ecp_core::analyzer::provider::LanguageProvider;
        use ecp_core::graph::{ArchivedZeroCopyGraph, RelType};
        use ecp_core::session::{MergedGraph, OverlayView, FRAGMENT_FORMAT_V4, FRAGMENT_FORMAT_V5};

        const CHILD_ENV: &str = "ECP_AUTO_ENSURE_V4_REFRESH_CHILD";
        const TEST_NAME: &str = "auto_ensure::fingerprint_drift_tests::test_retain_stale_fragments_native_v4_refresh_restores_call_edge";
        if std::env::var_os(CHILD_ENV).is_none() {
            for fingerprint in ["current", "released"] {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST_NAME, "--nocapture"])
                    .env(CHILD_ENV, fingerprint)
                    .status()
                    .unwrap();
                assert!(
                    status.success(),
                    "{fingerprint}: isolated test process failed: {status}"
                );
            }
            return;
        }

        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/overlay-written-by-0.16.2");
        let fragment = fs::read(fixture.join("graph_overlay/2e3d1df0cf371fbf.bin")).unwrap();
        let source = fs::read_to_string(fixture.join("app.py")).unwrap();
        let (tmp, gate) =
            gate_with_fragment(&fragment, "v4-refresh-test", FRAGMENT_FORMAT_V4, &source);
        assert_eq!(gate.rel_paths, ["app.py"]);
        let worktree = tmp.path().join("repo");
        let session_dir = resolve_session_overlay_dir(&worktree).unwrap();
        let gate = if std::env::var(CHILD_ENV).as_deref() == Ok("released") {
            let mut old =
                ecp_core::session::DirtyFiles::read(&fixture.join("dirty_files.json")).unwrap();
            let path = session_dir.join("dirty_files.json");
            old.entries.get_mut("app.py").unwrap().mtime_ns =
                mtime_ns_of(&worktree.join("app.py")).unwrap();
            let fragment_id = &old.entries["app.py"].fragment_id;
            fs::copy(
                fixture.join(format!("graph_overlay/{fragment_id}.bin")),
                session_dir.join(format!("graph_overlay/{fragment_id}.bin")),
            )
            .unwrap();
            assert!(!old.from_current_builder());
            assert_eq!(
                old.entries["app.py"].mtime_ns,
                mtime_ns_of(&worktree.join("app.py")).unwrap()
            );
            ecp_core::session::DirtyFiles::write_atomic(&path, &old).unwrap();
            let gate = retain_stale_fragments(&worktree, vec!["app.py".into()]);
            assert_eq!(gate.rel_paths, ["app.py"]);
            gate
        } else {
            gate
        };
        let provider = PythonProvider::new().unwrap();
        let local = provider
            .parse_file(Path::new("app.py"), source.as_bytes())
            .unwrap();
        let mtimes = HashMap::from([(
            "app.py".into(),
            mtime_ns_of(&worktree.join("app.py")).unwrap(),
        )]);
        apply_l1_overlay_updates(&worktree, &[local], &mtimes, gate.session_meta).unwrap();
        let manifest =
            ecp_core::session::DirtyFiles::read(&session_dir.join("dirty_files.json")).unwrap();
        assert_eq!(manifest.entries["app.py"].format, FRAGMENT_FORMAT_V5);
        assert!(manifest.from_current_builder());
        assert!(retain_stale_fragments(&worktree, vec!["app.py".into()])
            .rel_paths
            .is_empty());

        let (accepted, _) =
            crate::session::overlay_reader::accepted_fragments(&session_dir, &worktree).unwrap();
        let inputs = crate::session::overlay_reader::decode_view_inputs(&accepted);
        assert_eq!(inputs.len(), 1);
        let mut builder = GraphBuilder::new();
        builder.add_graph(
            provider
                .parse_file(Path::new("app.py"), b"def stable():\n    return 1\n")
                .unwrap(),
        );
        let base = builder.build();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
        let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let view = OverlayView::build(archived, &inputs).unwrap();
        let merged = MergedGraph::new(archived, Some(&view));
        assert!(merged.all_edges().any(|edge| {
            edge.rel_type() == RelType::Calls
                && merged.node(edge.source).unwrap().name(&merged) == "added"
                && merged.node(edge.target).unwrap().name(&merged) == "stable"
        }));
    }

    /// Re-runs `test_name` alone in a child process; returns false inside the
    /// child. The child sets `ECP_HOME` / `ECP_SESSION_ID` and reads the
    /// process-wide counters, which no sibling test may share.
    fn rerun_in_child(child_env: &str, test_name: &str) -> bool {
        if std::env::var_os(child_env).is_some() {
            return false;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(child_env, "1")
            .status()
            .unwrap();
        assert!(status.success(), "isolated test process failed: {status}");
        true
    }

    /// A repo at HEAD with one committed `app.py`, and a graph outside it
    /// whose sidecar names HEAD, so the porcelain shortcut decides. Returns
    /// the worktree and the graph path.
    fn repo_and_graph_at_head(tmp: &Path) -> (PathBuf, PathBuf) {
        let worktree = tmp.join("repo");
        fs::create_dir_all(&worktree).unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&worktree)
                .status()
                .unwrap()
                .success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "test"]);
        fs::write(worktree.join("app.py"), "def app(): pass\n").unwrap();
        git(&["add", "app.py"]);
        git(&["commit", "-qm", "base"]);
        let graph_path = tmp.join("cache").join("graph.bin");
        fs::create_dir_all(graph_path.parent().unwrap()).unwrap();
        let bytes = ecp_core::graph_fixture::GraphFixture::new().into_bytes();
        fs::write(&graph_path, &bytes).unwrap();
        write_head_sha_sidecar_with_sha(&graph_path, &git_head_sha(&worktree).unwrap());
        (worktree, graph_path)
    }

    /// `ensure_fresh` on `repo_and_graph_at_head` with `untracked` written
    /// after the commit. Returns the `reanalyze_files` calls and the paths
    /// they received.
    fn ensure_fresh_beside_untracked(session_id: &str, untracked: &[&str]) -> (usize, usize) {
        let tmp = tempdir().unwrap();
        let (worktree, graph_path) = repo_and_graph_at_head(tmp.path());
        std::env::set_var("ECP_HOME", tmp.path().join("ecp-home"));
        std::env::set_var("ECP_SESSION_ID", session_id);
        for name in untracked {
            fs::write(worktree.join(name), "x = 1\n").unwrap();
        }

        test_counters::reset();
        // The overlay write may fail in a bare temp dir; the counters move
        // before it.
        let _ = ensure_fresh(IndexNeed::NearCurrent, &graph_path, &worktree);
        (
            test_counters::reanalyze_calls(),
            test_counters::reanalyze_paths(),
        )
    }

    #[test]
    fn test_ensure_fresh_only_untracked_csv_skips_reanalyze() {
        if rerun_in_child(
            "ECP_AUTO_ENSURE_CSV_ONLY_CHILD",
            "auto_ensure::fingerprint_drift_tests::test_ensure_fresh_only_untracked_csv_skips_reanalyze",
        ) {
            return;
        }
        assert_eq!(
            ensure_fresh_beside_untracked("csv-only-test", &["a.csv", "b.csv", "c.csv"]),
            (0, 0),
            "untracked .csv files alone must not reach reanalyze_files"
        );
    }

    #[test]
    fn test_ensure_fresh_csv_beside_py_reanalyzes_py_only() {
        if rerun_in_child(
            "ECP_AUTO_ENSURE_CSV_PY_CHILD",
            "auto_ensure::fingerprint_drift_tests::test_ensure_fresh_csv_beside_py_reanalyzes_py_only",
        ) {
            return;
        }
        assert_eq!(
            ensure_fresh_beside_untracked("csv-py-test", &["a.csv", "new.py"]),
            (1, 1),
            "only the .py file may reach reanalyze_files"
        );
    }

    /// A `.json` file is source, so it survives `without_other`; under an
    /// `ECP_HOME` inside the worktree it is ecp's own file and must not make
    /// the graph stale.
    #[test]
    fn test_ensure_index_json_under_ecp_home_inside_worktree_returns_ready() {
        if rerun_in_child(
            "ECP_AUTO_ENSURE_ARTIFACT_CHILD",
            "auto_ensure::fingerprint_drift_tests::test_ensure_index_json_under_ecp_home_inside_worktree_returns_ready",
        ) {
            return;
        }
        let tmp = tempdir().unwrap();
        let (worktree, graph_path) = repo_and_graph_at_head(tmp.path());
        let home = worktree.join("ecp-home");
        fs::create_dir_all(home.join("repo")).unwrap();
        fs::write(home.join("repo/session_meta.json"), "{}\n").unwrap();
        std::env::set_var("ECP_HOME", &home);
        assert_eq!(
            ensure_index(&graph_path, &worktree).unwrap(),
            EnsureResult::Ready
        );
    }

    fn write_sidecar(graph_path: &Path, fp: &str) {
        fs::write(
            builder_fingerprint_sidecar_path(graph_path),
            format!("{fp}\n"),
        )
        .unwrap();
    }

    #[test]
    fn drift_detected_when_sidecar_fingerprint_differs() {
        let dir = tempdir().unwrap();
        let graph = dir.path().join("graph.bin");
        write_sidecar(&graph, "v0.3.0+schema1");
        assert!(
            fingerprint_drifted(&graph),
            "an older builder fingerprint must register as drifted"
        );
    }

    #[test]
    fn no_drift_when_sidecar_matches_current() {
        let dir = tempdir().unwrap();
        let graph = dir.path().join("graph.bin");
        write_sidecar(&graph, ecp_core::registry::BUILDER_FINGERPRINT);
        assert!(
            !fingerprint_drifted(&graph),
            "the current fingerprint must not register as drift"
        );
    }

    #[test]
    fn no_drift_when_fingerprint_unknown() {
        // Neither sidecar nor meta.json present: cannot prove drift, so defer
        // to the mtime walk rather than forcing a spurious full rebuild.
        let dir = tempdir().unwrap();
        let graph = dir.path().join("graph.bin");
        assert!(
            !fingerprint_drifted(&graph),
            "a missing fingerprint must NOT force a rebuild"
        );
    }

    #[test]
    fn drift_falls_back_to_meta_json_when_sidecar_absent() {
        // Graph built by a pre-sidecar binary: only meta.json carries the
        // fingerprint. ensure_index must still detect drift from it.
        let dir = tempdir().unwrap();
        let graph = dir.path().join("graph.bin");
        let meta = ecp_core::registry::CommitBuildMeta {
            version: 1,
            sha: "0".repeat(40),
            source_type: ecp_core::registry::SourceType::Branch,
            source_id: None,
            built_from_worktree: String::new(),
            built_at: String::new(),
            parent_sha: None,
            node_count: 0,
            embedding_status: ecp_core::registry::EmbeddingStatus::None,
            refs_at_build: vec![],
            refs_seen_since: vec![],
            builder_fingerprint: Some("v0.3.0+schema1".to_string()),
            binary_commit_sha: None,
        };
        ecp_core::registry::CommitBuildMeta::write_atomic(&dir.path().join("meta.json"), &meta)
            .unwrap();
        assert!(
            fingerprint_drifted(&graph),
            "drift must be detected from meta.json when the sidecar is absent"
        );
    }

    fn slot_meta(built_from_worktree: &str) -> ecp_core::registry::CommitBuildMeta {
        ecp_core::registry::CommitBuildMeta {
            version: 1,
            sha: "0".repeat(40),
            source_type: ecp_core::registry::SourceType::Branch,
            source_id: None,
            built_from_worktree: built_from_worktree.into(),
            built_at: String::new(),
            parent_sha: None,
            node_count: 0,
            embedding_status: ecp_core::registry::EmbeddingStatus::None,
            refs_at_build: vec![],
            refs_seen_since: vec![],
            builder_fingerprint: Some(ecp_core::registry::BUILDER_FINGERPRINT.to_string()),
            binary_commit_sha: None,
        }
    }

    /// A slot whose meta records a subdirectory of the worktree was published
    /// before `build_l2` resolved the root: it holds the subtree's graph only.
    /// Slots are immutable once published, so the verdict may be cached in a
    /// sidecar; each case therefore gets its own slot.
    #[test]
    fn ensure_index_rejects_slot_built_below_worktree_root() {
        let dir = tempdir().unwrap();
        let worktree = dir.path().join("repo");
        let subtree = worktree.join("crates").join("cli");
        fs::create_dir_all(&subtree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        let other = dir.path().join("other");
        fs::create_dir_all(&other).unwrap();
        let slot = |name: &str, built: &Path| {
            let slot = dir.path().join(name);
            fs::create_dir_all(&slot).unwrap();
            ecp_core::registry::CommitBuildMeta::write_atomic(
                &slot.join("meta.json"),
                &slot_meta(&built.to_string_lossy()),
            )
            .unwrap();
            slot.join("graph.bin")
        };

        let from_subtree = slot("from-subtree", &subtree);
        assert!(slot_built_below_root(&from_subtree, &worktree));
        assert!(
            worktree_sidecar_path(&from_subtree).is_file(),
            "the verdict source is back-filled as a sidecar"
        );
        assert!(
            slot_built_below_root(&from_subtree, &worktree),
            "the sidecar read gives the same verdict"
        );
        assert!(
            !slot_built_below_root(&from_subtree, &subtree),
            "the subtree itself is the root in use"
        );

        let from_root = slot("from-root", &worktree);
        assert!(!slot_built_below_root(&from_root, &worktree));
        fs::write(worktree_sidecar_path(&from_subtree), "").unwrap();
        assert!(
            slot_built_below_root(&from_subtree, &worktree),
            "a half-written sidecar falls back to meta.json"
        );
        assert!(
            !slot_built_below_root(&from_root, &other),
            "a slot from another worktree root of the same repo stays usable"
        );
    }
}

#[cfg(test)]
mod session_heartbeat_tests {
    use super::*;
    use tempfile::tempdir;

    fn write_meta(dir: &Path, last_touched: &str) -> PathBuf {
        let path = dir.join("session_meta.json");
        let meta = SessionMeta {
            version: 1,
            session_id: "sid".into(),
            pid: Some(999_999_999),
            started_at: last_touched.into(),
            last_touched: last_touched.into(),
            base_sha: "0".repeat(40),
            source_worktree: "/x".into(),
            overlay_version: 0,
            watcher_pid: None,
            last_drained_offset: 0,
            agent_name: None,
        };
        SessionMeta::write_atomic(&path, &meta).unwrap();
        path
    }

    fn age_file(path: &Path, secs: u64) {
        let t = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - std::time::Duration::from_secs(secs),
        );
        filetime::set_file_mtime(path, t).unwrap();
    }

    #[test]
    fn heartbeat_refreshes_last_touched_once_the_interval_has_passed() {
        let dir = tempdir().unwrap();
        let stamp = "2020-01-01T00:00:00+00:00";
        let path = write_meta(dir.path(), stamp);
        age_file(&path, SESSION_HEARTBEAT_SECS + 5);

        beat_meta_heartbeat(&path);

        let after = SessionMeta::read(&path).unwrap();
        assert_ne!(
            after.last_touched, stamp,
            "peers has no other evidence that this session is still active"
        );
        assert_eq!(after.started_at, stamp, "started_at is not a heartbeat");
    }

    /// A read-only query must beat too: an agent that edited an hour ago and
    /// has been reading since still holds those files dirty.
    #[test]
    fn heartbeat_does_not_enrol_a_session_that_never_existed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session_meta.json");
        beat_meta_heartbeat(&path);
        assert!(
            !path.exists(),
            "beating must never mint a session dir for a query-only agent"
        );
    }

    #[test]
    fn heartbeat_leaves_a_fresh_meta_untouched() {
        let dir = tempdir().unwrap();
        let stamp = "2020-01-01T00:00:00+00:00";
        let path = write_meta(dir.path(), stamp);

        beat_meta_heartbeat(&path);

        assert_eq!(
            SessionMeta::read(&path).unwrap().last_touched,
            stamp,
            "the query path must not pay a write per invocation"
        );
    }
}

#[cfg(test)]
mod rebuild_marker_tests {
    use super::*;

    /// F10: `spawn_bg` returning false used to be dropped on the floor; the
    /// next announcement now says the last rebuild failed and where its log is.
    #[test]
    fn test_spawn_background_rebuild_with_launcher_not_started_failed_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let markers = RebuildMarkers::under(tmp.path());
        std::fs::write(&markers.complete, b"").unwrap();
        spawn_background_rebuild_with(tmp.path(), &markers, |_| false);
        assert!(markers.failed.exists());
        assert!(!markers.complete.exists());
        let log = std::fs::read_to_string(&markers.log).unwrap();
        assert!(log.contains("rebuild launcher did not start"), "{log}");
        assert_eq!(
            markers.last_failed_note(),
            format!(" last=failed log={}", markers.log.display())
        );
    }

    /// The job gets all three markers, under the repo root and never in the
    /// worktree it rebuilds.
    #[test]
    fn test_spawn_background_rebuild_with_markers_under_repo_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("home").join("repo");
        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let markers = RebuildMarkers::under(&repo_root);
        let mut seen = Vec::new();
        spawn_background_rebuild_with(&worktree, &markers, |job| {
            let m = job.markers.as_ref().expect("the job reports its outcome");
            seen = vec![
                job.lock.to_path_buf(),
                m.log.to_path_buf(),
                m.complete.to_path_buf(),
                m.failed.to_path_buf(),
            ];
            true
        });
        assert_eq!(seen.len(), 4);
        assert!(
            seen.iter().all(|p| p.parent() == Some(repo_root.as_path())),
            "{seen:?}"
        );
        assert!(!markers.failed.exists());
        assert_eq!(std::fs::read_dir(&worktree).unwrap().count(), 0);
    }

    #[test]
    fn test_last_failed_note_no_failed_marker_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(RebuildMarkers::under(tmp.path()).last_failed_note(), "");
    }
}
