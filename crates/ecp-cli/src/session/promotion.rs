//! Promotion: handle HEAD drift mid-session. Case A (fast-forward,
//! content-equivalence drop) preserves dirty deltas the commit didn't
//! absorb. Case B (cross-refactor, atomic invalidate) wipes L1 clean
//! and restarts from the new base.

use crate::git::safe_exec;
use ecp_core::registry::{atomic_write_json, FileLock};
use ecp_core::session::layout;
use ecp_core::session::{DirtyEvents, SessionMeta};
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
    // The writer commits bins and manifest under this lock; promotion rewrites
    // the same manifest and deletes bins, so an unlocked run could drop an
    // entry a writer just added or delete a bin it just published.
    let _publish =
        FileLock::acquire_exclusive(&super::overlay_writer::overlay_lock_path(session_dir))?;
    let manifest_path = layout::dirty_events_path(session_dir);
    // A manifest that does not parse is re-parsed whole by the freshness
    // gate, so it promotes as empty. Failing here instead would fail every
    // refresh and leave it in place.
    let mut df = DirtyEvents::read(&manifest_path)
        .unwrap_or_else(|_| DirtyEvents::empty(ecp_core::config::resolve_size_cap(worktree)));

    let entries_snapshot: Vec<(String, _)> = df
        .entries
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut dropped = 0usize;
    let mut kept = 0usize;
    for (rel_path, entry) in entries_snapshot {
        let at_new_sha = git_cat_file_hash(worktree, new_sha, &rel_path);
        let absorbed = match (&at_new_sha, &entry.fragment_id) {
            // A tombstone: the commit absorbed the deletion when the path is
            // gone at new_sha too.
            (Err(_), None) => true,
            (Ok(_), None) => false,
            // File doesn't exist at new_sha (e.g. deleted in commit) — keep
            // the L1 fragment so the session still sees a view.
            (Err(_), Some(_)) => false,
            (Ok(l2_hash), Some(_)) => entry.content_hash.as_deref() == Some(l2_hash.as_str()),
        };
        if absorbed {
            // Drop fragment + manifest entry.
            if let Some(id) = &entry.fragment_id {
                let _ = fs::remove_file(layout::fragment_path(session_dir, id));
            }
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
    DirtyEvents::write_atomic(&manifest_path, &df)?;

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
    let (prev_worktree, prev_agent_name) =
        SessionMeta::read(&session_dir.join("session_meta.json"))
            .map(|sm| (sm.source_worktree, sm.agent_name))
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
        agent_name: prev_agent_name,
    };
    let size_cap = ecp_core::config::resolve_size_cap(Path::new(&sm.source_worktree));
    atomic_write_json(&session_dir.join("session_meta.json"), &sm)?;
    DirtyEvents::write_atomic(
        &layout::dirty_events_path(session_dir),
        &DirtyEvents::empty(size_cap),
    )?;

    // Background GC stale dir after 2s. The thread is detached — we don't
    // join it; if the process exits sooner, the .stale dir is left for the
    // next ecp admin gc sweep (Phase 7) to pick up.
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

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::contracts::dirty_event::{PathChange, PathClass};
    use ecp_core::session::DirtyEventEntry;

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = safe_exec::git()
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn fragment(content: &[u8]) -> DirtyEventEntry {
        let hash = crate::repo_identity::xxh3_hex16(content);
        DirtyEventEntry {
            change: PathChange::Modified,
            class: PathClass::SOURCE,
            mtime_ns: 1,
            fragment_id: Some(hash.clone()),
            content_hash: Some(hash),
            parse_failed: false,
            dirty_symbols: Vec::new(),
            format: ecp_core::session::FRAGMENT_FORMAT_V7,
        }
    }

    /// Case A reads `l1/dirty_events.json`: the entry the commit absorbed
    /// goes with its bin, the uncommitted one stays, a tombstone of a
    /// deletion the commit absorbed goes, and the legacy manifest an older
    /// binary wrote is left as it was. Case B resets the new manifest.
    #[test]
    fn test_promotion_moves_new_namespace() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@example.invalid"]);
        git(&repo, &["config", "user.name", "t"]);
        for name in ["a.py", "gone.py"] {
            fs::write(repo.join(name), "v1\n").unwrap();
        }
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        let old_sha = git(&repo, &["rev-parse", "HEAD"]);

        let session = tmp.path().join("sessions").join("sid");
        fs::create_dir_all(&session).unwrap();
        let meta = SessionMeta {
            version: 1,
            session_id: "sid".into(),
            pid: None,
            started_at: String::new(),
            last_touched: String::new(),
            base_sha: old_sha.clone(),
            source_worktree: repo.to_string_lossy().into(),
            overlay_version: 0,
            watcher_pid: None,
            last_drained_offset: 0,
            agent_name: None,
        };
        SessionMeta::write_atomic(&session.join("session_meta.json"), &meta).unwrap();
        let mut manifest = DirtyEvents::empty(ecp_core::config::resolve_size_cap(&repo));
        let absorbed = fragment(b"v2\n");
        let absorbed_bin =
            layout::fragment_path(&session, absorbed.fragment_id.as_deref().unwrap());
        fs::create_dir_all(absorbed_bin.parent().unwrap()).unwrap();
        fs::write(&absorbed_bin, b"bin").unwrap();
        manifest.entries.insert("a.py".into(), absorbed);
        manifest
            .entries
            .insert("c.py".into(), fragment(b"uncommitted\n"));
        manifest.entries.insert(
            "gone.py".into(),
            DirtyEventEntry::tombstone(PathClass::SOURCE),
        );
        DirtyEvents::write_atomic(&layout::dirty_events_path(&session), &manifest).unwrap();
        let legacy = br#"{"version":1,"entries":{}}"#;
        fs::write(layout::legacy_dirty_files_path(&session), legacy).unwrap();

        fs::write(repo.join("a.py"), "v2\n").unwrap();
        fs::remove_file(repo.join("gone.py")).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "absorb"]);
        let new_sha = git(&repo, &["rev-parse", "HEAD"]);
        assert_eq!(promotion_case(&old_sha, &new_sha, &repo), PromotionCase::A);

        let stats = promote_case_a(&session, &repo, &new_sha).unwrap();

        let after = DirtyEvents::read(&layout::dirty_events_path(&session)).unwrap();
        let keys: Vec<&str> = after.entries.keys().map(String::as_str).collect();
        assert_eq!(keys, ["c.py"]);
        assert_eq!((stats.dropped, stats.kept), (2, 1));
        assert!(
            !absorbed_bin.exists(),
            "the absorbed entry's bin stays behind"
        );
        assert_eq!(
            fs::read(layout::legacy_dirty_files_path(&session)).unwrap(),
            legacy,
            "promotion rewrote the legacy manifest"
        );

        promote_case_b(&session, &new_sha, "f".repeat(40).as_str()).unwrap();

        let reset = DirtyEvents::read(&layout::dirty_events_path(&session)).unwrap();
        assert!(reset.entries.is_empty());
        assert!(reset.is_current(ecp_core::config::resolve_size_cap(&repo)));
        assert!(!layout::legacy_dirty_files_path(&session).exists());
    }
}
