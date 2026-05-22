//! Auto-ensure index for agent CLI commands.
//!
//! `ensure_index` reports state (Ready / Stale / Missing). `ensure_fresh`
//! is the actionable wrapper:
//!
//! - Missing → full L2 build via `build_l2` (sync, cold path).
//! - Stale, header-incompatible → full L2 rebuild (corrupt overlay = corruption risk).
//! - Stale, header-compatible → incremental: `reanalyze_files` for dirty set,
//!   then L1 overlay fragment write under `<repo>/sessions/<sid>/`.
//! - Ready → noop.

use ecp_core::session::SessionMeta;
use ignore::WalkBuilder;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
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

    pub fn reset() {
        REANALYZE_CALL_COUNT.store(0, Ordering::Relaxed);
        BUILD_L2_CALL_COUNT.store(0, Ordering::Relaxed);
    }

    pub fn reanalyze_calls() -> usize {
        REANALYZE_CALL_COUNT.load(Ordering::Relaxed)
    }

    pub fn build_l2_calls() -> usize {
        BUILD_L2_CALL_COUNT.load(Ordering::Relaxed)
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
    Stale { age_seconds: u64 },
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
        return Ok(EnsureResult::Stale { age_seconds: 0 });
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
        return Ok(EnsureResult::Stale { age_seconds: age });
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
        .args(["status", "--porcelain", "--untracked-files=no"])
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
    Some(EnsureResult::Stale { age_seconds: age })
}

/// Ensure the graph exists and is fresher than the working tree.
///
/// - Missing → `build_l2` (sync, L2 cold path).
/// - Stale, header-incompatible → `build_l2` (full rebuild; applying an L1
///   overlay against an incompatible base schema produces a corrupt graph).
/// - Stale, header-compatible → incremental: `reanalyze_files` for the dirty
///   file set (T7-4), followed by L1 overlay fragment write (T7-5 will replace
///   the overlay write with a zero-copy in-place merge).
/// - Ready → noop.
pub fn ensure_fresh(graph_path: &Path, worktree_root: &Path) -> Result<(), String> {
    let state =
        ensure_index(graph_path, worktree_root).map_err(|e| format!("ensure_index probe: {e}"))?;
    match state {
        EnsureResult::Ready => Ok(()),
        EnsureResult::Missing => {
            let start = std::time::Instant::now();
            // build_l2 → build_inside_locked writes the HEAD-SHA sidecar in
            // the background as its final step; no extra write needed here.
            crate::build::orchestrator::build_l2(worktree_root, None)
                .map_err(|e| format!("build_l2: {e}"))?;
            eprintln!("l2.built elapsed={:.2}s", start.elapsed().as_secs_f32());
            Ok(())
        }
        EnsureResult::Stale { .. } => {
            if !crate::engine::header_compatible(graph_path) {
                // Version-incompatible base: applying an overlay would silently
                // corrupt graph.bin. Fully rebuild instead.
                // Invariant (T1-7 + OQ-5): this branch must NEVER call the overlay path.
                // Counter is incremented before build_l2 so tests can assert branch
                // dispatch even when build_l2 fails in a minimal tempdir fixture.
                test_counters::BUILD_L2_CALL_COUNT
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let start = std::time::Instant::now();
                crate::build::orchestrator::build_l2(worktree_root, None)
                    .map_err(|e| format!("build_l2 (incompatible schema): {e}"))?;
                eprintln!("l2.rebuilt elapsed={:.2}s", start.elapsed().as_secs_f32());
            } else {
                // Header-compatible + dirty files: incremental refresh path (T7-4).
                //
                // Step 1 — reanalyze the dirty file set.
                // `reanalyze_files` returns fresh `LocalGraph` views for each
                // changed file. T7-5 will consume these to do a zero-copy
                // in-place merge; for now the results are intentionally unused
                // here — the wiring into auto_ensure is the T7-4 deliverable.
                let graph_mtime = fs::metadata(graph_path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                let dirty_abs =
                    collect_dirty_files(graph_path, worktree_root, graph_mtime).unwrap_or_default();
                let rel_paths: Vec<String> = dirty_abs
                    .iter()
                    .filter_map(|p| p.strip_prefix(worktree_root).ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                let _fresh_graphs = crate::reanalyze::reanalyze_files(
                    worktree_root,
                    &crate::git::DiffScope::Unstaged,
                    &rel_paths,
                );
                test_counters::REANALYZE_CALL_COUNT
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // Step 2 — write L1 overlay fragments for the dirty set.
                // Reuse the `dirty_abs` already collected above instead of
                // re-walking the tree (~10k extra stat syscalls on a mid-size
                // repo). T7-5 will replace the overlay write with a zero-copy
                // merge of `_fresh_graphs` directly into graph.bin.
                apply_l1_overlay_updates(worktree_root, dirty_abs)
                    .map_err(|e| format!("L1 overlay refresh: {e}"))?;
                // L1 overlay only touches dirty fragments under
                // `<repo>/sessions/<sid>/` and does not rewrite graph.bin, but the
                // working tree is now consistent with the indexed state of the
                // current HEAD — refresh the fingerprint as the very last step.
                write_head_sha_sidecar(graph_path, worktree_root);
            }
            Ok(())
        }
    }
}

fn apply_l1_overlay_updates(worktree_root: &Path, dirty_files: Vec<PathBuf>) -> io::Result<()> {
    use crate::session::{overlay_writer, promotion, resolver};

    let session_id = resolver::resolve_session_id(None);
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let repo_dir = crate::repo_identity::repo_dir_name_for_cwd(worktree_root)?;
    let session_dir = home_ecp.join(&repo_dir).join("sessions").join(&session_id);
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

    // `dirty_files` was collected once by `ensure_fresh` and threaded through
    // — see comment at the call site for the rationale.

    // Build inputs first so per-file read errors get individually surfaced
    // (matches the old per-file warning), then commit fragment writes +
    // manifest + version in one batched call. Coalesces 2N atomic_write_json
    // calls (dirty_files.json + session_meta.json per file) down to 2.
    let mut inputs: Vec<overlay_writer::FragmentInput> = Vec::with_capacity(dirty_files.len());
    for dirty_path in &dirty_files {
        let rel = dirty_path
            .strip_prefix(worktree_root)
            .unwrap_or(dirty_path.as_path());
        let content = match fs::read(dirty_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("warning: overlay read for {}: {e}", rel.display());
                continue;
            }
        };
        let mtime_ns = fs::metadata(dirty_path)?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        inputs.push(overlay_writer::FragmentInput {
            rel_path: rel.to_string_lossy().into(),
            content,
            mtime_ns,
        });
    }

    let (n_written, n_failed) =
        match overlay_writer::write_dirty_fragments_batch(&session_dir, &inputs) {
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
const SKIP_DIRS: &[&str] = &[".git", ".ecp", ".ecp"];

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

    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .add_custom_ignore_filename(crate::ECP_IGNORE_FILE)
        .require_git(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !SKIP_DIRS.contains(&name.as_ref())
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

    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .add_custom_ignore_filename(crate::ECP_IGNORE_FILE)
        .require_git(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !SKIP_DIRS.contains(&name.as_ref())
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
