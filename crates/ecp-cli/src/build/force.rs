//! Force rebuild orchestration: drop existing L2 + selective L1 invalidation
//! before re-running the standard build pipeline.

use crate::build::dirname_picker::pick_dirname;
use crate::build::orchestrator::{
    acquire_sha_lock, attach_if_fingerprint_matches, build_inside_locked, parse_sha,
    published_during_wait, report_wait, worktree_toplevel, BuildResult, DurablePublish, Holder,
    Waited, BUILD_LOCK_TIMEOUT,
};
use crate::commit_lookup::{CommitDirListing, CommitIndex};
use crate::repo_identity::repo_dir_name_for_cwd;
use crate::session::state::classify_with_index;
use ecp_core::registry::{resolve_home_ecp, retire_dir_async, SourceType};
use ecp_core::session::{SessionState, StaleReason};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct InvalidateReport {
    pub kept: usize,
    pub invalidated: usize,
    pub stale_skipped: usize,
    /// `MetaUnreadable` sessions reaped on sight: no `session_meta.json` means
    /// no `base_sha`, so they can never serve a query nor be rebuilt — keeping
    /// them only grows `~/.ecp` unbounded across binary upgrades. Distinct from
    /// `stale_skipped` (corrupt-but-recoverable) and `invalidated` (live, this
    /// SHA).
    pub meta_reaped: usize,
}

/// Retire each `sessions/<sid>/` whose `SessionState` is `AugmentedReference`
/// with `base_sha == target_sha`, and return counts. PureReference sessions for the
/// same SHA are kept. Stale sessions are left for the GC sweep.
pub fn invalidate_matching_l1(repo_root: &Path, target_sha: &str) -> io::Result<InvalidateReport> {
    let sessions_dir = repo_root.join("sessions");
    if !sessions_dir.exists() {
        return Ok(InvalidateReport::default());
    }
    let mut report = InvalidateReport::default();
    // Hoist CommitIndex::scan once — classify_with_index reuses it across all
    // sessions instead of re-walking commits/ per session. scan_cached lets
    // back-to-back --force invocations also skip the readdir.
    let idx = CommitIndex::scan_cached(&repo_root.join("commits")).ok();
    let idx_ref = idx.as_deref();

    for entry in fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') || name.contains(".stale-") || name.contains(".dead") {
            continue;
        }

        match classify_with_index(repo_root, name, idx_ref) {
            SessionState::PureReference { base_sha, .. } if base_sha == target_sha => {
                report.kept += 1;
            }
            SessionState::AugmentedReference { base_sha, .. } if base_sha == target_sha => {
                // `retire_dir_async` rather than a `.stale-<sha8>` of our own:
                // that name is the same on every run, so a second `--force` at
                // one SHA renamed onto the directory the first left behind and
                // died with ENOTEMPTY. The retired name carries pid, counter
                // and timestamp, so it cannot collide, and `admin gc` already
                // sweeps that shape.
                retire_dir_async(&path)?;
                report.invalidated += 1;
            }
            // MetaUnreadable is unrecoverable: no session_meta.json ⇒ no
            // base_sha ⇒ never serveable, never rebuildable, unrelated to any
            // target SHA. Reap on sight (no sha-hint gate) instead of warning
            // forever. See FU-2026-05-25-006.
            SessionState::Stale {
                reason: StaleReason::MetaUnreadable,
            } => {
                retire_dir_async(&path)?;
                report.meta_reaped += 1;
            }
            SessionState::Stale { reason } if matches_sha_hint(repo_root, name, target_sha) => {
                tracing::warn!(
                    "session={} stale ({:?}) during force rebuild — skipping (use `admin sessions reset`)",
                    name,
                    reason
                );
                report.stale_skipped += 1;
            }
            _ => {}
        }
    }
    Ok(report)
}

/// Stale-state sessions don't expose base_sha through `classify`. Read raw
/// `session_meta.json` to decide whether this stale session is even in scope
/// for the current `--force`. Read failure ⇒ count as in-scope (conservative).
fn matches_sha_hint(repo_root: &Path, sid: &str, target_sha: &str) -> bool {
    let path = repo_root
        .join("sessions")
        .join(sid)
        .join("session_meta.json");
    match ecp_core::session::SessionMeta::read(&path) {
        Ok(sm) => sm.base_sha == target_sha,
        Err(_) => true,
    }
}

pub struct ForceRebuildResult {
    pub sha_hex: String,
    pub source_type: SourceType,
    /// Asserted by `tests/build_orchestrator.rs` and `tests/force_rebuild_test.rs`
    /// to verify the published `graph.bin` / `meta.json` location. Bin caller
    /// (`admin/index.rs::run`) prints summary fields only.
    #[allow(dead_code)]
    pub commit_dir: PathBuf,
    /// Asserted by `tests/force_rebuild_test.rs` to distinguish a real
    /// rebuild from a fingerprint-match attach. Bin caller does not branch
    /// on it.
    #[allow(dead_code)]
    pub rebuilt: bool,
    pub invalidate_report: InvalidateReport,
    /// Propagated from the inner `BuildResult.tantivy_handle`. See
    /// `BuildResult::join_background`. `None` for the attach short-circuit
    /// (no rebuild → no background tantivy run).
    pub tantivy_handle: Option<std::thread::JoinHandle<()>>,
}

impl ForceRebuildResult {
    pub fn join_background(&mut self) {
        if let Some(h) = self.tantivy_handle.take() {
            if let Err(panic) = h.join() {
                tracing::warn!("tantivy background thread panicked: {panic:?}");
            }
        }
    }
}

/// --force orchestration. See spec §4.2.
///
/// Order: SHA lock (attach if a newer build was published while waiting) →
/// invalidate matching L1 sessions → drop the parse cache → the shared build
/// and publish, which keeps the SHA lock through the rename. L1-before-L2
/// keeps crash recovery self-consistent.
pub fn force_rebuild_l2(worktree: &Path, target_sha: &str) -> io::Result<ForceRebuildResult> {
    // Same rule as build_l2: the slot is keyed by sha, so a subdirectory must
    // not publish its subtree as the commit's graph.
    let toplevel = worktree_toplevel(worktree);
    let worktree = toplevel.as_deref().unwrap_or(worktree);
    let sha = parse_sha(target_sha)?;
    let repo_root = resolve_home_ecp().join(repo_dir_name_for_cwd(worktree)?);
    force_rebuild_l2_at(worktree, &sha, &repo_root, report_wait)
}

/// [`force_rebuild_l2`] for a resolved worktree root, SHA and repo root.
pub(crate) fn force_rebuild_l2_at(
    worktree: &Path,
    sha: &[u8; 20],
    repo_root: &Path,
    on_wait: impl FnOnce(&Holder),
) -> io::Result<ForceRebuildResult> {
    let sha_hex = hex::encode(sha);
    let commits_dir = repo_root.join("commits");
    fs::create_dir_all(&commits_dir)?;
    let base = pick_dirname(worktree, &sha_hex)?;

    let (sha_lock, waited) = acquire_sha_lock(&commits_dir, sha, BUILD_LOCK_TIMEOUT, on_wait)?;
    // N concurrent --force collapse into one build: a waiter attaches to what
    // the holder published during the wait. Only a build that is provably
    // newer than the one current before the wait counts; the build that was
    // already current is exactly what --force was asked to replace.
    if let Waited::Yes(before) = waited {
        let after = CommitDirListing::read(&commits_dir);
        let attached = after
            .as_ref()
            .ok()
            .and_then(|after| published_during_wait(&before, after, sha, &commits_dir))
            .and_then(|winner| {
                attach_if_fingerprint_matches(&commits_dir.join(&winner.name), worktree)
            });
        if let Some(attached) = attached {
            return Ok(ForceRebuildResult {
                sha_hex: attached.sha_hex,
                source_type: attached.source_type,
                commit_dir: attached.commit_dir,
                rebuilt: false,
                invalidate_report: InvalidateReport::default(),
                tantivy_handle: attached.tantivy_handle,
            });
        }
    }

    // 2. Invalidate matching L1 BEFORE publishing the new L2 (spec §4.4)
    let invalidate_report = invalidate_matching_l1(repo_root, &sha_hex)?;

    // 3. Keep existing L2 in place. The rebuild publishes a same-SHA
    // generation dir so active readers can continue mmaping the old graph.

    // 3.5. Drop per-file parse_cache too.
    //
    // The cache at `<repo_root>/parse_cache/<fingerprint>/<content_hash>.rkyv`
    // keys entries by (BUILDER_FINGERPRINT, file content). Fingerprint only
    // bumps with `CARGO_PKG_VERSION` or the `+schemaN` literal, so dev-loop
    // analyzer / queries.scm edits don't invalidate cache entries — `--force`
    // rebuilt L2 but reused stale parser outputs for unchanged-content files.
    // For `--force` (the "I want a fresh build" lever), drop the whole cache
    // dir so the next build re-parses every file with the current binary.
    // Stale-fingerprint orphan subdirs disappear with this too, which is OK
    // since they were unreachable already.
    let _ = retire_dir_async(&repo_root.join("parse_cache"))?;

    // 4-8. Shared build pipeline (source → analyzer → meta → durable publish
    // → repo_meta). It releases the SHA lock after the publish.
    let BuildResult {
        sha_hex,
        source_type,
        commit_dir,
        tantivy_handle,
    } = build_inside_locked(worktree, sha, repo_root, &base, sha_lock, &DurablePublish)?;

    Ok(ForceRebuildResult {
        sha_hex,
        source_type,
        commit_dir,
        rebuilt: true,
        tantivy_handle,
        invalidate_report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::orchestrator::test_support::*;
    use crate::commit_lookup::sha_lock_path;
    use ecp_core::registry::FileLock;

    /// Nothing else holds the SHA: --force replaces even a build that the
    /// current binary made, and the new build is the one readers pick.
    #[test]
    fn test_force_rebuild_l2_at_uncontended_matching_build_rebuilds() {
        let fx = fx();
        let first = build_at(&fx, &fx.repo_root);
        let mut r = force_rebuild_l2_at(&fx.wt, &fx.sha, &fx.repo_root, no_wait).unwrap();
        r.join_background();
        assert!(r.rebuilt);
        assert_ne!(r.commit_dir, first.commit_dir);
        assert_eq!(
            current_name(&fx).as_deref(),
            r.commit_dir.file_name().and_then(|n| n.to_str())
        );
    }

    /// Hold the SHA lock, start a --force that waits on it, publish `winner`
    /// into `commits/` once the waiter has taken its before-wait snapshot
    /// (`publish_during_wait`) or before it starts, then release.
    fn force_while_held(fx: &Fx, winner: &Path, publish_during_wait: bool) -> ForceRebuildResult {
        let published = fx.commits.join(winner.file_name().unwrap());
        if !publish_during_wait {
            fs::rename(winner, &published).unwrap();
        }
        let held = FileLock::acquire_exclusive(&sha_lock_path(&fx.commits, &fx.sha)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut r = std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                force_rebuild_l2_at(&fx.wt, &fx.sha, &fx.repo_root, move |_: &Holder| {
                    tx.send(()).unwrap()
                })
            });
            rx.recv().expect("the waiter reports its wait");
            if publish_during_wait {
                fs::rename(winner, &published).unwrap();
            }
            drop(held);
            waiter.join().unwrap().unwrap()
        });
        r.join_background();
        r
    }

    /// F4: a matching build published while this --force waited is the
    /// result of a concurrent --force; this one attaches instead of building
    /// the same thing again.
    #[test]
    fn test_force_rebuild_l2_at_published_during_wait_attaches() {
        let fx = fx();
        let built = build_at(&fx, &fx.tmp.path().join("home").join("elsewhere"));
        let r = force_while_held(&fx, &built.commit_dir, true);
        assert!(!r.rebuilt);
        assert_eq!(
            r.commit_dir,
            fx.commits.join(built.commit_dir.file_name().unwrap())
        );
        assert_eq!(CommitDirListing::read(&fx.commits).unwrap().dirs.len(), 1);
    }

    /// F4: the winner was already current when the waiter looked, so nothing
    /// proves it is a concurrent --force rather than the build this --force
    /// was asked to replace. The waiter builds: one extra build, never a
    /// skipped rebuild.
    #[test]
    fn test_force_rebuild_l2_at_published_before_snapshot_rebuilds() {
        let fx = fx();
        let built = build_at(&fx, &fx.tmp.path().join("home").join("elsewhere"));
        let r = force_while_held(&fx, &built.commit_dir, false);
        assert!(r.rebuilt);
        assert_eq!(CommitDirListing::read(&fx.commits).unwrap().dirs.len(), 2);
        assert_eq!(
            current_name(&fx).as_deref(),
            r.commit_dir.file_name().and_then(|n| n.to_str())
        );
    }
}
