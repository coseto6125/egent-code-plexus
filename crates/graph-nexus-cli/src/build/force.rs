//! Force rebuild orchestration: drop existing L2 + selective L1 invalidation
//! before re-running the standard build pipeline.

use crate::session::state::classify;
use graph_nexus_core::session::SessionState;
use std::fs;
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
    let sha8 = &target_sha[..8];
    let mut report = InvalidateReport::default();

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

        match classify(repo_root, name) {
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
