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

use ecp_core::session::SessionMeta;
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::fs;
use std::io;
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
    pub static BUILD_L2_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// FU-2026-05-23-047: incremented every time `ensure_fresh` synchronously
    /// joins the background tantivy writer because `~/.ecp/` resolves to a
    /// path inside the current worktree. Observed by the regression test in
    /// `tests/ensure_fresh_tantivy_drain.rs`; in normal prod (cache root
    /// sibling to the repo) this counter stays at 0.
    pub static TANTIVY_JOIN_COUNT: AtomicUsize = AtomicUsize::new(0);
    /// Incremented every time `ensure_fresh` returns `WarmAttach` instead of
    /// blocking on a cold build. Lets integration tests assert the fast path
    /// fires for OOB branch-switch scenarios.
    pub static WARM_ATTACH_COUNT: AtomicUsize = AtomicUsize::new(0);

    pub fn reset() {
        REANALYZE_CALL_COUNT.store(0, Ordering::Relaxed);
        BUILD_L2_CALL_COUNT.store(0, Ordering::Relaxed);
        TANTIVY_JOIN_COUNT.store(0, Ordering::Relaxed);
        WARM_ATTACH_COUNT.store(0, Ordering::Relaxed);
    }

    pub fn reanalyze_calls() -> usize {
        REANALYZE_CALL_COUNT.load(Ordering::Relaxed)
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
    /// shortcut, the exact changed paths it already parsed (absolute) — lets
    /// `ensure_fresh` skip the whole-tree mtime walk in `collect_dirty_files`
    /// (O(changed) vs O(tree): ~0.86s → ~0.02s on a 14k-file repo). `None` when
    /// the decision came from the mtime walk (no path list available) or a
    /// full-rebuild drift (dirty set is irrelevant).
    Stale {
        age_seconds: u64,
        needs_full_rebuild: bool,
        dirty_files: Option<Vec<PathBuf>>,
    },
}

/// Outcome returned by `ensure_fresh`, disambiguating the warm-attach fast
/// path from a fully synchronous build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureFreshOutcome {
    /// Graph is up-to-date (or was synchronously rebuilt / overlaid). No
    /// special action needed; `graph_path::resolve` will find the right graph.
    Ready,
    /// New HEAD has no published graph yet. The most-recent sibling SHA's graph
    /// is usable for this invocation; a background rebuild for the new SHA has
    /// been spawned. The caller should load `sibling_graph_path` and mark the
    /// engine stale so LLM consumers know results may be slightly behind.
    WarmAttach { sibling_graph_path: PathBuf },
}

pub fn ensure_index(graph_path: &Path, worktree_root: &Path) -> io::Result<EnsureResult> {
    let graph_mtime = match fs::metadata(graph_path) {
        Ok(m) => m.modified()?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(EnsureResult::Missing),
        Err(e) => return Err(e),
    };

    // A CLI upgrade that bumped GRAPH_FORMAT_VERSION would otherwise surface
    // as engine::Engine::load's InvalidData error on the next query. Treat a
    // schema break the same as a stale graph so ensure_fresh transparently
    // rebuilds.
    if !crate::engine::header_compatible(graph_path) {
        return Ok(EnsureResult::Stale {
            age_seconds: 0,
            needs_full_rebuild: true,
            dirty_files: None,
        });
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
        return Ok(EnsureResult::Stale {
            age_seconds: 0,
            needs_full_rebuild: true,
            dirty_files: None,
        });
    }

    // Fast path: if the working tree is a git repo, the indexed HEAD matches
    // the current HEAD, and `git status --porcelain -uno` is empty, the graph
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
        return Ok(EnsureResult::Stale {
            age_seconds: age,
            needs_full_rebuild: false,
            // mtime walk only answers "is anything newer", not "which files";
            // ensure_fresh falls back to collect_dirty_files for the path list.
            dirty_files: None,
        });
    }
    Ok(EnsureResult::Ready)
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
/// a second `git rev-parse`. The 41-byte write is detached into a background
/// thread so callers (build / attach / overlay) never block on the sidecar —
/// a write failure (permissions, full disk, process exits mid-syscall) just
/// means the next `ensure_index` falls back to the existing mtime walk. The
/// sidecar is a perf hint, not a correctness requirement.
pub fn write_head_sha_sidecar_with_sha(graph_path: &Path, sha: &str) {
    let sidecar = head_sha_sidecar_path(graph_path);
    let content = format!("{sha}\n");
    std::thread::spawn(move || {
        let _ = fs::write(&sidecar, content);
    });
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

/// Write the current `GRAPH_FORMAT_VERSION` next to `graph_path`. Detached so
/// the build wall-clock isn't bumped; failure is silently dropped because
/// `attach_latest_sibling_sha` falls back to `header_compatible` when the
/// sidecar is missing — perf hint, not a correctness requirement.
pub fn write_compatible_version_sidecar(graph_path: &Path) {
    let sidecar = compatible_version_sidecar_path(graph_path);
    let content = format!("{}\n", ecp_core::graph::GRAPH_FORMAT_VERSION);
    std::thread::spawn(move || {
        let _ = fs::write(&sidecar, content);
    });
}

fn read_compatible_version_sidecar(graph_path: &Path) -> Option<u32> {
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

/// Write the current `BUILDER_FINGERPRINT` next to `graph_path`. Unlike the
/// head-SHA and compatible-version sidecars — pure read-side perf hints whose
/// miss merely falls back to an mtime walk / `header_compatible` — a stale or
/// missing fingerprint sidecar makes `fingerprint_drifted` report drift and
/// triggers a full `build_l2` (the most expensive fallback, not a cheap one).
/// So this write is synchronous: on return the sidecar reflects the running
/// binary and drift is cleared. Detaching the ~20-byte write to a spawned
/// thread risks the process exiting before the flush lands — the next
/// invocation would then rebuild an already-current graph — and the spawn
/// itself costs more than the write it would defer.
pub fn write_builder_fingerprint_sidecar(graph_path: &Path) {
    let sidecar = builder_fingerprint_sidecar_path(graph_path);
    let content = format!("{}\n", ecp_core::registry::BUILDER_FINGERPRINT);
    let _ = fs::write(&sidecar, content);
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

/// Try to decide Ready vs Stale via the cheap git fingerprint.
/// - Returns `Some(Ready)` when HEAD matches the sidecar AND `git status
///   --porcelain -uno` is empty.
/// - Returns `Some(Stale)` when HEAD matches but the working tree has
///   uncommitted changes (skip walk; let L1 overlay collect dirty files).
/// - Returns `None` when the fingerprint check is inconclusive (not a git
///   repo, sidecar missing, HEAD differs, or git command failed) so the
///   caller falls back to the full mtime walk.
fn git_fingerprint_shortcut(
    graph_path: &Path,
    worktree_root: &Path,
    graph_mtime: SystemTime,
) -> Option<EnsureResult> {
    let sidecar_sha = fs::read_to_string(head_sha_sidecar_path(graph_path))
        .ok()?
        .trim()
        .to_string();
    if sidecar_sha.is_empty() {
        return None;
    }
    let head_sha = git_head_sha(worktree_root).ok()?;
    if sidecar_sha != head_sha {
        return None;
    }

    let porcelain = crate::git::safe_exec::git()
        // `-z` (NUL-terminated) avoids git's C-quoting of paths with spaces or
        // non-ASCII bytes, which the human-readable format would otherwise wrap
        // in double quotes and escape — producing a path that doesn't exist on
        // disk and silently dropping that file from the incremental refresh.
        .args(["status", "--porcelain", "-z", "--untracked-files=no"])
        .current_dir(worktree_root)
        .output()
        .ok()?;
    if !porcelain.status.success() {
        return None;
    }
    if porcelain.stdout.is_empty() {
        return Some(EnsureResult::Ready);
    }
    let age = SystemTime::now()
        .duration_since(graph_mtime)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // git already listed the exact changed paths here — carry them so
    // ensure_fresh skips the whole-tree mtime walk in collect_dirty_files.
    let dirty_files = parse_porcelain_paths(&porcelain.stdout, worktree_root);
    Some(EnsureResult::Stale {
        age_seconds: age,
        needs_full_rebuild: false,
        dirty_files: Some(dirty_files),
    })
}

/// Parse `git status --porcelain -z` stdout into absolute paths under `root`.
///
/// In `-z` mode entries are NUL-terminated and paths are NOT quoted, so a
/// filename with spaces or non-ASCII bytes round-trips intact. A status entry
/// is `XY <path>` (2-char code + space); a rename emits the new path in that
/// entry and the old path as a separate trailing NUL segment with no status
/// prefix. We keep every segment that carries the `XY ` prefix (the new path
/// for renames — the one that now exists on disk) and skip prefix-less old-path
/// segments. Any spurious path still degrades to "not reanalyzed" because
/// `reanalyze_files` skips paths that don't exist on disk.
fn parse_porcelain_paths(stdout: &[u8], root: &Path) -> Vec<PathBuf> {
    stdout
        .split(|&b| b == 0)
        // Keep only segments shaped like `XY <path>`: ≥4 bytes with a space in
        // the status separator slot. Prefix-less rename old-paths fail this and
        // are dropped.
        .filter(|seg| seg.len() >= 4 && seg[2] == b' ')
        .map(|seg| String::from_utf8_lossy(&seg[3..]).into_owned())
        .filter(|p| !p.is_empty())
        .map(|p| root.join(p))
        .collect()
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
pub fn ensure_fresh(graph_path: &Path, worktree_root: &Path) -> Result<EnsureFreshOutcome, String> {
    let state =
        ensure_index(graph_path, worktree_root).map_err(|e| format!("ensure_index probe: {e}"))?;
    match state {
        EnsureResult::Ready => Ok(EnsureFreshOutcome::Ready),
        EnsureResult::Missing => {
            if let Some(sibling) = attach_latest_sibling_sha(worktree_root) {
                spawn_background_rebuild(worktree_root);
                test_counters::WARM_ATTACH_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "l2.warm-attach sibling={} rebuild=background",
                    sibling.display()
                );
                return Ok(EnsureFreshOutcome::WarmAttach {
                    sibling_graph_path: sibling,
                });
            }
            let start = std::time::Instant::now();
            // build_l2 → build_inside_locked writes the HEAD-SHA sidecar in
            // the background as its final step; no extra write needed here.
            let mut result = crate::build::orchestrator::build_l2(worktree_root, None)
                .map_err(|e| format!("build_l2: {e}"))?;
            drain_tantivy_if_inside_worktree(&mut result, worktree_root);
            eprintln!("l2.built elapsed={:.2}s", start.elapsed().as_secs_f32());
            Ok(EnsureFreshOutcome::Ready)
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
                let mut result = crate::build::orchestrator::build_l2(worktree_root, None)
                    .map_err(|e| format!("build_l2 (incompatible schema): {e}"))?;
                drain_tantivy_if_inside_worktree(&mut result, worktree_root);
                eprintln!("l2.rebuilt elapsed={:.2}s", start.elapsed().as_secs_f32());
            } else {
                // Header-compatible + dirty files: incremental refresh path.
                //
                // Prefer the dirty set git already produced in the porcelain
                // shortcut (O(changed)); only fall back to the whole-tree mtime
                // walk when the staleness came from the mtime path itself.
                let dirty_abs = match dirty_files {
                    Some(paths) => paths,
                    None => {
                        let graph_mtime = fs::metadata(graph_path)
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::UNIX_EPOCH);
                        collect_dirty_files(graph_path, worktree_root, graph_mtime)
                            .unwrap_or_default()
                    }
                };
                let rel_paths: Vec<String> = dirty_abs
                    .iter()
                    .filter_map(|p| p.strip_prefix(worktree_root).ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                // Parse the dirty set ONCE here; the resulting `LocalGraph`s are
                // handed straight to the overlay writer, which no longer
                // re-parses them. This removes the second full tree-sitter pass
                // (the dominant incremental cost on a large repo: ~0.6s for the
                // 20-provider pipeline init alone).
                let fresh_graphs = crate::reanalyze::reanalyze_files(
                    worktree_root,
                    &crate::git::DiffScope::Unstaged,
                    &rel_paths,
                );
                test_counters::REANALYZE_CALL_COUNT
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                apply_l1_overlay_updates(worktree_root, &fresh_graphs)
                    .map_err(|e| format!("L1 overlay refresh: {e}"))?;
                // L1 overlay only touches dirty fragments under
                // `<repo>/sessions/<sid>/` and does not rewrite graph.bin, but the
                // working tree is now consistent with the indexed state of the
                // current HEAD — refresh the fingerprint as the very last step.
                write_head_sha_sidecar(graph_path, worktree_root);
            }
            Ok(EnsureFreshOutcome::Ready)
        }
    }
}

/// Version-checked graph load for every path that loads a graph by repo
/// (find multi-repo, group, diff) rather than by explicit `--graph`.
///
/// Runs `ensure_fresh` first so the two-tier staleness contract applies
/// uniformly: a header/fingerprint drift forces a full `build_l2` before the
/// load (else the caller would read nodes a stale binary produced — e.g. a
/// parser bug already fixed upstream), and a dirty working tree triggers the
/// incremental path. On `WarmAttach` (current SHA has no graph yet) it loads
/// the sibling graph and flags it stale so the caller can surface a note.
///
/// `--graph <path>` is the one documented opt-out and must NOT route through
/// here: it loads the named graph verbatim via `Engine::load`, since the user
/// is deliberately pointing at a specific graph (e.g. A/B graph comparison).
pub fn load_ensured(
    graph_path: &Path,
    worktree_root: &Path,
) -> Result<crate::engine::Engine, String> {
    match ensure_fresh(graph_path, worktree_root)? {
        EnsureFreshOutcome::Ready => crate::engine::Engine::load(graph_path)
            .map_err(|e| format!("load graph {}: {e}", graph_path.display())),
        EnsureFreshOutcome::WarmAttach { sibling_graph_path } => {
            crate::engine::Engine::load_warm(&sibling_graph_path).map_err(|e| {
                format!(
                    "load warm-attach graph {}: {e}",
                    sibling_graph_path.display()
                )
            })
        }
    }
}

/// Process-lifetime cache for `attach_latest_sibling_sha` results, keyed by
/// `worktree_root`. CLI processes call once per invocation so the cache is a
/// no-op; long-lived MCP processes repeating ensure_fresh while a rebuild is
/// in flight see all subsequent calls short-circuit. Once the rebuild lands,
/// the new SHA's graph short-circuits `ensure_fresh` itself before reaching
/// this path, so cache staleness across rebuilds is harmless.
static SIBLING_CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<PathBuf>>>> = OnceLock::new();

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
        if !graph_bin.is_file() || !sibling_graph_compatible(&graph_bin) {
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
    let Some(dir_name) = sibling_dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Ok(parsed) = ecp_core::registry::CommitDirName::parse(dir_name) else {
        return false;
    };
    let sibling_sha = hex::encode(parsed.sha);
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

/// Fire-and-forget background rebuild for `worktree_root`.
///
/// Uses the same `spawn_bg` + `flock -n` pattern as `post_tool_use`'s
/// `spawn_background_reindex`. Concurrent triggers no-op because `flock -n`
/// exits immediately if another builder holds the lock.
fn spawn_background_rebuild(worktree_root: &Path) {
    // Integration tests exercise the warm-attach outcome + counter but have no
    // use for the detached rebuild; skipping it avoids a leaked `sh` subprocess
    // that nextest flags as LEAK on Windows (the rebuild outlives the test).
    if std::env::var_os("ECP_SKIP_BG_REBUILD").is_some() {
        return;
    }
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let Ok(repo_dir_name) = crate::repo_identity::repo_dir_name_for_cwd(worktree_root) else {
        return;
    };
    let lock = home_ecp
        .join(&repo_dir_name)
        .join(".warm-attach-rebuild.lock");
    let repo_str = worktree_root.to_string_lossy();
    let args = ["admin", "index", "--repo", repo_str.as_ref()];
    let _ = crate::background::spawn_bg(crate::background::BgJob {
        args: &args,
        lock: &lock,
        cwd: worktree_root,
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
) {
    let cache_root = ecp_core::registry::resolve_home_ecp();
    if cache_root.starts_with(worktree_root) {
        result.join_background();
        test_counters::TANTIVY_JOIN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Resolve the L1 session overlay dir for `worktree_root`, matching exactly the
/// path `apply_l1_overlay_updates` writes to: `<home_ecp>/<repo_dir>/sessions/
/// <session_id>`. Returns `None` when the repo identity can't be resolved. The
/// dir may not exist yet (no overlay written this session) — the caller checks.
pub fn resolve_session_overlay_dir(worktree_root: &Path) -> Option<PathBuf> {
    let session_id = crate::session::resolver::resolve_session_id(None);
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let repo_dir = crate::repo_identity::repo_dir_name_for_cwd(worktree_root).ok()?;
    Some(home_ecp.join(&repo_dir).join("sessions").join(&session_id))
}

fn apply_l1_overlay_updates(
    worktree_root: &Path,
    fresh_graphs: &[ecp_core::analyzer::types::LocalGraph],
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
    let session_meta = SessionMeta::read(&sm_path)?;
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
    // writer consumes them directly (no second tree-sitter pass). We only need
    // each file's mtime — a cheap stat the graph doesn't carry. `file_path` is
    // relative to the worktree root.
    let mtimes: Vec<u64> = fresh_graphs
        .iter()
        .map(|g| {
            fs::metadata(worktree_root.join(&g.file_path))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
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

fn ensure_session_meta(session_dir: &Path, worktree: &Path) -> io::Result<()> {
    let meta_path = session_dir.join("session_meta.json");
    if meta_path.exists() {
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

    #[test]
    fn parse_porcelain_z_handles_spaces_and_renames() {
        let root = Path::new("/repo");
        // modified file with a space in its name + a rename (new\0old).
        let stdout = b" M src/a b.ts\0R  src/new.ts\0src/old.ts\0 M src/c.ts\0";
        let got = parse_porcelain_paths(stdout, root);
        // new path of the rename is kept; the prefix-less old path is dropped.
        assert_eq!(
            got,
            vec![
                PathBuf::from("/repo/src/a b.ts"),
                PathBuf::from("/repo/src/new.ts"),
                PathBuf::from("/repo/src/c.ts"),
            ]
        );
    }

    #[test]
    fn parse_porcelain_z_empty_is_empty() {
        assert!(parse_porcelain_paths(b"", Path::new("/repo")).is_empty());
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
}
