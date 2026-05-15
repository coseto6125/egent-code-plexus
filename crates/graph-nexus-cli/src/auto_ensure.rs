//! Auto-ensure index for agent CLI commands.
//!
//! Protocol (see spec §5):
//!   1. If graph.bin missing → caller should trigger `admin index` synchronously
//!      and retry. Returned as `EnsureResult::Missing`.
//!   2. If graph.bin present but mtime < newest source file → emit stale warning
//!      to stderr (caller continues). Returned as `EnsureResult::Stale { age_seconds }`.
//!   3. Otherwise → `EnsureResult::Ready`.
//!
//! This module checks status; it does NOT invoke the index build (callers
//! decide whether to auto-build or surface the missing state — `cypher` may
//! prefer to fail, `inspect` will auto-build).

use ignore::WalkBuilder;
use std::fs;
use std::io;
use std::path::Path;
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

    if let Some(src_mtime) = newest_source_mtime(graph_path, worktree_root)? {
        if src_mtime > graph_mtime {
            let age = SystemTime::now()
                .duration_since(graph_mtime)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return Ok(EnsureResult::Stale { age_seconds: age });
        }
    }
    Ok(EnsureResult::Ready)
}

fn newest_source_mtime(graph_path: &Path, root: &Path) -> io::Result<Option<SystemTime>> {
    let graph_canonical = fs::canonicalize(graph_path).ok();
    let mut newest: Option<SystemTime> = None;

    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git" | "target" | "node_modules" | ".gitnexus-rs"
            )
        })
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
    {
        // Skip graph.bin itself to avoid self-comparison
        let skip = graph_canonical.as_deref().map_or(false, |gc| {
            fs::canonicalize(entry.path()).ok().as_deref() == Some(gc)
        });
        if skip {
            continue;
        }

        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                newest = match newest {
                    None => Some(mtime),
                    Some(curr) if mtime > curr => Some(mtime),
                    _ => newest,
                };
            }
        }
    }
    Ok(newest)
}
