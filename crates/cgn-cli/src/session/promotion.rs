//! Promotion: handle HEAD drift mid-session. Case A (fast-forward,
//! content-equivalence drop) preserves dirty deltas the commit didn't
//! absorb. Case B (cross-refactor, atomic invalidate) wipes L1 clean
//! and restarts from the new base.

use crate::git::safe_exec;
use graph_nexus_core::registry::atomic_write_json;
use graph_nexus_core::session::{DirtyFiles, SessionMeta};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionCase {
    A,
    B,
}

pub struct PromoteStats {
    pub dropped: usize,
    pub kept: usize,
}

pub fn promotion_case(old_sha: &str, new_sha: &str, worktree: &Path) -> PromotionCase {
    if old_sha == new_sha {
        // No drift — caller shouldn't have invoked promotion.
        // Default to A (no-op): subsequent promote_case_a is idempotent.
        return PromotionCase::A;
    }
    match merge_base(old_sha, new_sha, worktree) {
        Some(b) if b == old_sha => PromotionCase::A,
        _ => PromotionCase::B,
    }
}

pub fn promote_case_a(
    session_dir: &Path,
    worktree: &Path,
    new_sha: &str,
) -> io::Result<PromoteStats> {
    let manifest_path = session_dir.join("dirty_files.json");
    let mut df = if manifest_path.exists() {
        DirtyFiles::read(&manifest_path)?
    } else {
        DirtyFiles::empty()
    };

    let entries_snapshot: Vec<(String, _)> = df
        .entries
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut dropped = 0usize;
    let mut kept = 0usize;
    for (rel_path, entry) in entries_snapshot {
        let l2_hash = match git_cat_file_hash(worktree, new_sha, &rel_path) {
            Ok(h) => h,
            Err(_) => {
                // File doesn't exist at new_sha (e.g. deleted in commit) —
                // keep the L1 fragment so the session still sees a view.
                kept += 1;
                continue;
            }
        };
        if entry.content_hash == l2_hash {
            // Drop fragment + manifest entry.
            let fragment_path = session_dir
                .join("graph_overlay")
                .join(format!("{}.bin", entry.fragment_id));
            let _ = fs::remove_file(&fragment_path);
            df.entries.remove(&rel_path);
            dropped += 1;
        } else {
            kept += 1;
        }
    }

    // Update session_meta.base_sha.
    let sm_path = session_dir.join("session_meta.json");
    if sm_path.exists() {
        let mut sm = SessionMeta::read(&sm_path)?;
        sm.base_sha = new_sha.to_string();
        sm.last_touched = chrono::Utc::now().to_rfc3339();
        atomic_write_json(&sm_path, &sm)?;
    }
    atomic_write_json(&manifest_path, &df)?;

    Ok(PromoteStats { dropped, kept })
}

pub fn promote_case_b(session_dir: &Path, old_sha: &str, new_sha: &str) -> io::Result<()> {
    let parent = session_dir
        .parent()
        .ok_or_else(|| io::Error::other("session_dir has no parent"))?;
    let sid = session_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::other("invalid session_dir name"))?
        .to_string();
    let stale = parent.join(format!("{sid}.stale-{old_sha}"));

    // Rescue existing source_worktree from old session_meta if present.
    // If reading fails, fall back to empty string — the auto_ensure path
    // that triggers promotion will write a fresh session_meta with the real
    // worktree on the next L1 write.
    let prev_worktree = SessionMeta::read(&session_dir.join("session_meta.json"))
        .map(|sm| sm.source_worktree)
        .unwrap_or_default();

    fs::rename(session_dir, &stale)?;
    fs::create_dir(session_dir)?;

    let now = chrono::Utc::now().to_rfc3339();
    let sm = SessionMeta {
        version: 1,
        session_id: sid,
        pid: Some(std::process::id()),
        started_at: now.clone(),
        last_touched: now,
        base_sha: new_sha.to_string(),
        source_worktree: prev_worktree,
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    atomic_write_json(&session_dir.join("session_meta.json"), &sm)?;
    atomic_write_json(&session_dir.join("dirty_files.json"), &DirtyFiles::empty())?;

    // Background GC stale dir after 2s. The thread is detached — we don't
    // join it; if the process exits sooner, the .stale dir is left for the
    // next gnx admin gc sweep (Phase 7) to pick up.
    let stale_clone = stale.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let _ = fs::remove_dir_all(&stale_clone);
    });

    Ok(())
}

fn merge_base(a: &str, b: &str, worktree: &Path) -> Option<String> {
    let out = safe_exec::git()
        .args(["merge-base", a, b])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(std::str::from_utf8(&out.stdout).ok()?.trim().to_string())
}

fn git_cat_file_hash(worktree: &Path, sha: &str, rel_path: &str) -> io::Result<String> {
    let spec = format!("{sha}:{rel_path}");
    let out = safe_exec::git()
        .args(["cat-file", "blob", &spec])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("git cat-file failed for {spec}")));
    }
    Ok(crate::repo_identity::xxh3_hex16(&out.stdout))
}
