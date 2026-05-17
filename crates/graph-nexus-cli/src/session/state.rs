//! `classify`: pure function from filesystem state to `SessionState`.
//! Lives in cli (not core) because resolving `base_sha → l2_dirname` requires
//! `commit_lookup::CommitIndex`, which is a cli-side concern.

use crate::commit_lookup::CommitIndex;
use graph_nexus_core::session::{DirtyFiles, SessionMeta, SessionState, StaleReason};
use std::fs;
use std::path::Path;

pub fn classify(repo_root: &Path, sid: &str) -> SessionState {
    let sid_dir = repo_root.join("sessions").join(sid);
    let sm_path = sid_dir.join("session_meta.json");
    let sm = match SessionMeta::read(&sm_path) {
        Ok(sm) => sm,
        Err(_) => return SessionState::Stale { reason: StaleReason::MetaUnreadable },
    };

    let l2_dirname = match resolve_l2_dirname(repo_root, &sm.base_sha) {
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
            base_sha: sm.base_sha,
            l2_dirname,
        }
    } else {
        SessionState::AugmentedReference {
            base_sha: sm.base_sha,
            l2_dirname,
            fragment_count: dirty.entries.len(),
        }
    }
}

fn resolve_l2_dirname(repo_root: &Path, sha_hex: &str) -> Option<String> {
    let commits = repo_root.join("commits");
    let idx = CommitIndex::scan(&commits).ok()?;
    let sha_bytes = sha_hex_to_bytes(sha_hex)?;
    idx.find(&sha_bytes).map(|s| s.to_string())
}

fn sha_hex_to_bytes(hex: &str) -> Option<[u8; 20]> {
    if hex.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}
