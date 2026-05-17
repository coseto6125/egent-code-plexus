//! `classify`: pure function from filesystem state to `SessionState`.
//! Lives in cli (not core) because resolving `base_sha → l2_dirname` requires
//! `commit_lookup::CommitIndex`, which is a cli-side concern.

use crate::commit_lookup::CommitIndex;
use graph_nexus_core::session::{DirtyFiles, SessionMeta, SessionState, StaleReason};
use std::fs;
use std::path::Path;

pub fn classify(repo_root: &Path, sid: &str) -> SessionState {
    // scan_cached: per-process cache keyed on commits/ mtime. Cuts
    // hot-path Engine::open from N readdir/query down to N stat/query
    // when many queries hit the same SHA. Arc deref gives back &CommitIndex.
    let idx = CommitIndex::scan_cached(&repo_root.join("commits")).ok();
    classify_with_index(repo_root, sid, idx.as_deref())
}

/// Hot-loop variant: callers that classify multiple sessions for the same
/// repo can scan `CommitIndex` once and pass it in, avoiding N × readdir.
/// `None` means "no commits dir / scan failed" — every session becomes
/// `Stale(L2Missing)`.
pub(crate) fn classify_with_index(
    repo_root: &Path,
    sid: &str,
    idx: Option<&CommitIndex>,
) -> SessionState {
    let sm_path = repo_root.join("sessions").join(sid).join("session_meta.json");
    let sm = match SessionMeta::read(&sm_path) {
        Ok(sm) => sm,
        Err(_) => return SessionState::Stale { reason: StaleReason::MetaUnreadable },
    };
    classify_with_meta(repo_root, sid, &sm, idx)
}

/// Maximum-reuse variant: caller has already read `SessionMeta` for other
/// fields (e.g. last_touched) and passes it in to avoid a second open.
/// Used by `admin sessions list::collect_rows`.
pub(crate) fn classify_with_meta(
    repo_root: &Path,
    sid: &str,
    sm: &SessionMeta,
    idx: Option<&CommitIndex>,
) -> SessionState {
    let sid_dir = repo_root.join("sessions").join(sid);

    let l2_dirname = match resolve_l2_dirname_with(idx, &sm.base_sha) {
        Some(d) => d,
        None => return SessionState::Stale { reason: StaleReason::L2Missing },
    };

    let dirty_path = sid_dir.join("dirty_files.json");
    let dirty = match fs::read(&dirty_path) {
        Ok(bytes) => match serde_json::from_slice::<DirtyFiles>(&bytes) {
            Ok(df) => df,
            Err(_) => return SessionState::Stale { reason: StaleReason::DirtyFilesCorrupt },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirtyFiles::empty(),
        Err(_) => return SessionState::Stale { reason: StaleReason::DirtyFilesCorrupt },
    };

    if dirty.entries.is_empty() {
        SessionState::PureReference {
            base_sha: sm.base_sha.clone(),
            l2_dirname,
        }
    } else {
        SessionState::AugmentedReference {
            base_sha: sm.base_sha.clone(),
            l2_dirname,
            fragment_count: dirty.entries.len(),
        }
    }
}

fn resolve_l2_dirname_with(idx: Option<&CommitIndex>, sha_hex: &str) -> Option<String> {
    let sha_bytes = sha_hex_to_bytes(sha_hex)?;
    idx?.find(&sha_bytes).map(|s| s.to_string())
}

pub(crate) fn sha_hex_to_bytes(s: &str) -> Option<[u8; 20]> {
    hex::decode(s).ok()?.try_into().ok()
}
