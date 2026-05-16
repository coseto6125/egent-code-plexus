//! Auto-ensure index for agent CLI commands.
//!
//! `ensure_index` reports state (Ready / Stale / Missing). `ensure_fresh`
//! is the actionable wrapper: if state is Stale or Missing it invokes
//! `admin index` synchronously, prints a one-line stderr notice once the
//! rebuild succeeds, then returns. Agent commands call `ensure_fresh`
//! before loading the graph so the user never sees a "stale" warning or
//! a "graph.bin not found" failure for a tracked worktree.

use ignore::WalkBuilder;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Instant, SystemTime};

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

    if any_source_newer_than(graph_path, worktree_root, graph_mtime)? {
        let age = SystemTime::now()
            .duration_since(graph_mtime)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return Ok(EnsureResult::Stale { age_seconds: age });
    }
    Ok(EnsureResult::Ready)
}

/// Ensure the graph exists and is fresher than the working tree. On Missing
/// or Stale, invokes `admin index` synchronously for `worktree_root`, prints
/// a one-line "Index refreshed" notice to stderr, and returns. Ready returns
/// immediately with no output. Errors from the rebuild surface verbatim.
pub fn ensure_fresh(graph_path: &Path, worktree_root: &Path) -> Result<(), String> {
    let state =
        ensure_index(graph_path, worktree_root).map_err(|e| format!("ensure_index probe: {e}"))?;
    let reason = match state {
        EnsureResult::Ready => return Ok(()),
        EnsureResult::Missing => "missing",
        EnsureResult::Stale { .. } => "stale",
    };

    let start = Instant::now();
    let args = crate::commands::admin::index::IndexArgs {
        repo: worktree_root.to_string_lossy().into_owned(),
        force: false,
        dump_resolver: None,
        no_cache: false,
        quiet: true,
    };
    crate::commands::admin::index::run(args)?;
    eprintln!(
        "✓ Index refreshed ({} → fresh in {:.1}s)",
        reason,
        start.elapsed().as_secs_f32(),
    );
    Ok(())
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
