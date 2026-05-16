//! Auto-ensure index for agent CLI commands.
//!
//! `ensure_index` reports state (Ready / Stale / Missing). `ensure_fresh`
//! is the actionable wrapper:
//!
//! - Missing → full L2 build via `build_l2` (sync, cold path).
//! - Stale → per-file L1 overlay update; only dirty files are re-parsed
//!   and written as fragments under `<repo>/sessions/<sid>/`.
//! - Ready → noop.

use graph_nexus_core::session::SessionMeta;
use ignore::WalkBuilder;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

    if any_source_newer_than(graph_path, worktree_root, graph_mtime)? {
        let age = SystemTime::now()
            .duration_since(graph_mtime)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return Ok(EnsureResult::Stale { age_seconds: age });
    }
    Ok(EnsureResult::Ready)
}

/// Returns `true` iff the graph at `graph_path` exists, loads cleanly,
/// and has an embeddings table. Used by reindex spawn sites to decide
/// whether to pass `--embeddings` to the new build — preserving the
/// previous state so a `git commit` doesn't silently disable vector /
/// hybrid search on the next query.
///
/// Any failure (missing file, corrupt rkyv, mmap error) returns `false`
/// — the caller falls back to a non-embedded rebuild rather than
/// erroring out the hook path.
pub fn embeddings_present(graph_path: &Path) -> bool {
    let Ok(engine) = crate::engine::Engine::load(graph_path) else {
        return false;
    };
    let Ok(graph) = engine.graph() else {
        return false;
    };
    graph.embeddings.is_some()
}

/// Ensure the graph exists and is fresher than the working tree.
///
/// - Missing → `build_l2` (sync, L2 cold path).
/// - Stale → per-file L1 overlay refresh under `<repo>/sessions/<sid>/`.
/// - Ready → noop.
pub fn ensure_fresh(graph_path: &Path, worktree_root: &Path) -> Result<(), String> {
    let state =
        ensure_index(graph_path, worktree_root).map_err(|e| format!("ensure_index probe: {e}"))?;
    match state {
        EnsureResult::Ready => Ok(()),
        EnsureResult::Missing => {
            let start = std::time::Instant::now();
            crate::build::orchestrator::build_l2(worktree_root, None)
                .map_err(|e| format!("build_l2: {e}"))?;
            eprintln!(
                "✓ Index built (L2 cold path in {:.1}s)",
                start.elapsed().as_secs_f32()
            );
            Ok(())
        }
        EnsureResult::Stale { .. } => apply_l1_overlay_updates(graph_path, worktree_root)
            .map_err(|e| format!("L1 overlay refresh: {e}")),
    }
}

fn apply_l1_overlay_updates(graph_path: &Path, worktree_root: &Path) -> io::Result<()> {
    use crate::session::{overlay_writer, resolver};

    let session_id = resolver::resolve_session_id(None);
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let repo_dir = crate::repo_identity::repo_dir_name_for_cwd(worktree_root)?;
    let session_dir = home_gnx.join(&repo_dir).join("sessions").join(&session_id);
    fs::create_dir_all(&session_dir)?;
    ensure_session_meta(&session_dir, worktree_root)?;

    let graph_mtime = fs::metadata(graph_path)?.modified()?;
    let dirty_files = collect_dirty_files(graph_path, worktree_root, graph_mtime)?;

    let mut n_written = 0usize;
    let mut n_failed = 0usize;
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
        let input = overlay_writer::FragmentInput {
            rel_path: rel.to_string_lossy().into(),
            content,
            mtime_ns,
        };
        match overlay_writer::write_dirty_fragment(&session_dir, &input) {
            Ok(o) if o.parse_failed => n_failed += 1,
            Ok(_) => n_written += 1,
            Err(e) => eprintln!("warning: overlay write for {}: {e}", rel.display()),
        }
    }
    if n_written > 0 || n_failed > 0 {
        eprintln!("✓ L1 overlay refreshed ({n_written} written, {n_failed} parse-failed)");
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
    };
    SessionMeta::write_atomic(&meta_path, &sm)
}

fn git_head_sha(worktree: &Path) -> io::Result<String> {
    let out = crate::git::safe_exec::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("git rev-parse HEAD failed"));
    }
    Ok(std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_string())
}

/// Build artifacts, vendor dirs, and language-specific caches: walking
/// these is wasted work, and their mtimes are noisy (touched by tools,
/// not source changes), so excluding them avoids false-positive Stale.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".gnx",
    "__pycache__",
    ".venv",
    "venv",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".cache",
];

/// Short-circuits on the first source file newer than `graph_mtime`.
/// Walking the full tree only happens when the graph is genuinely
/// fresh — Stale repos exit early at first hit.
fn any_source_newer_than(
    graph_path: &Path,
    root: &Path,
    graph_mtime: SystemTime,
) -> io::Result<bool> {
    let graph_canonical = fs::canonicalize(graph_path).ok();

    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
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
        .git_ignore(false)
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
