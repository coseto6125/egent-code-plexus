//! Force rebuild orchestration: drop existing L2 + selective L1 invalidation
//! before re-running the standard build pipeline.

use crate::build::dirname_picker::pick_dirname;
use crate::build::orchestrator::{build_inside_locked, wait_for_completion, BuildResult};
use crate::commit_lookup::CommitIndex;
use crate::repo_identity::repo_dir_name_for_cwd;
use crate::session::state::classify_with_index;
use fs2::FileExt;
use graph_nexus_core::registry::{resolve_home_gnx, SourceType};
use graph_nexus_core::session::SessionState;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

#[derive(Debug, Default, Clone)]
pub struct InvalidateReport {
    pub kept: usize,
    pub invalidated: usize,
    pub stale_skipped: usize,
}

/// Rename each `sessions/<sid>/` whose `SessionState` is `AugmentedReference`
/// with `base_sha == target_sha` to `sessions/<sid>.stale-<sha8>/`, spawn a
/// 2s delayed `rm -rf`, and return counts. PureReference sessions for the
/// same SHA are kept. Stale sessions are left for the GC sweep.
pub fn invalidate_matching_l1(repo_root: &Path, target_sha: &str) -> io::Result<InvalidateReport> {
    let sessions_dir = repo_root.join("sessions");
    if !sessions_dir.exists() {
        return Ok(InvalidateReport::default());
    }
    let sha8 = target_sha.get(..8).unwrap_or(target_sha);
    let mut report = InvalidateReport::default();
    // Hoist CommitIndex::scan once — classify_with_index reuses it across all
    // sessions instead of re-walking commits/ per session.
    let idx = CommitIndex::scan(&repo_root.join("commits")).ok();

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

        match classify_with_index(repo_root, name, idx.as_ref()) {
            SessionState::PureReference { base_sha, .. } if base_sha == target_sha => {
                report.kept += 1;
            }
            SessionState::AugmentedReference { base_sha, .. } if base_sha == target_sha => {
                let stale_path = sessions_dir.join(format!("{name}.stale-{sha8}"));
                fs::rename(&path, &stale_path)?;
                spawn_delayed_rm_rf(stale_path, Duration::from_secs(2));
                report.invalidated += 1;
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
    let path = repo_root.join("sessions").join(sid).join("session_meta.json");
    match graph_nexus_core::session::SessionMeta::read(&path) {
        Ok(sm) => sm.base_sha == target_sha,
        Err(_) => true,
    }
}

fn spawn_delayed_rm_rf(path: PathBuf, delay: Duration) {
    thread::spawn(move || {
        thread::sleep(delay);
        let _ = fs::remove_dir_all(&path);
    });
}

#[derive(Debug)]
pub struct ForceRebuildResult {
    pub sha_hex: String,
    pub source_type: SourceType,
    pub commit_dir: PathBuf,
    pub rebuilt: bool,
    pub invalidate_report: InvalidateReport,
}

/// --force orchestration. See spec §4.2.
///
/// Order: acquire lock (attach-and-retake if contended) → invalidate matching
/// L1 sessions → drop existing L2 → re-run build pipeline → atomic publish.
/// L1-before-L2 keeps crash recovery self-consistent.
pub fn force_rebuild_l2(worktree: &Path, target_sha: &str) -> io::Result<ForceRebuildResult> {
    let sha_hex = target_sha.to_string();
    if sha_hex.len() != 40 || !sha_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(io::Error::other(format!("invalid sha: {sha_hex}")));
    }

    let home_gnx = resolve_home_gnx();
    let repo_dir_name = repo_dir_name_for_cwd(worktree)?;
    let repo_root = home_gnx.join(&repo_dir_name);
    fs::create_dir_all(repo_root.join("commits"))?;

    let dirname = pick_dirname(worktree, &sha_hex)?;
    let commit_dir = repo_root.join("commits").join(&dirname);
    let building = repo_root
        .join("commits")
        .join(format!("{dirname}.building"));

    // 1. Acquire lock (attach-and-retake pattern if contended).
    // Hold the lock fd in `lock_guard` until the function returns — fs2 advisory
    // locks release on fd close, so dropping mid-function would let a concurrent
    // --force race into our drop+rebuild window.
    fs::create_dir_all(&building)?;
    let lock_path = building.join(".build.lock");
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    let lock_guard = if lock.try_lock_exclusive().is_err() {
        wait_for_completion(&building, &commit_dir)?;
        fs::create_dir_all(&building)?;
        let lock2 = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)?;
        lock2
            .try_lock_exclusive()
            .map_err(|e| io::Error::other(format!("re-lock after attach failed: {e}")))?;
        lock2
    } else {
        lock
    };

    // 2. Invalidate matching L1 BEFORE dropping L2 (spec §4.4)
    let invalidate_report = invalidate_matching_l1(&repo_root, &sha_hex)?;

    // 3. Drop existing L2
    if commit_dir.exists() {
        fs::remove_dir_all(&commit_dir)?;
    }

    // 4-8. Shared build pipeline (source → analyzer → meta → atomic publish → repo_meta)
    let BuildResult {
        sha_hex,
        source_type,
        commit_dir,
    } = build_inside_locked(worktree, &sha_hex, &repo_root, &building, &commit_dir)?;

    drop(lock_guard); // explicit release after publish

    Ok(ForceRebuildResult {
        sha_hex,
        source_type,
        commit_dir,
        rebuilt: true,
        invalidate_report,
    })
}
