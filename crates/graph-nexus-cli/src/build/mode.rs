//! Build mode decision: Sync iff no completed L2 entries exist for the repo.
//!
//! Rationale: first build for a repo MUST be sync because no fallback L2 exists
//! to serve queries while the build runs. Subsequent builds (SHA drift) can be
//! background — caller can serve from old L2 + L1 overlay until new L2 lands.

use crate::commit_lookup::CommitIndex;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    None,
    Sync,
    Background,
}

pub fn build_mode(repo_root: &Path, target_sha: &[u8; 20]) -> BuildMode {
    let commits = repo_root.join("commits");
    let idx = match CommitIndex::scan(&commits) {
        Ok(i) => i,
        Err(_) => return BuildMode::Sync,
    };
    if idx.find(target_sha).is_some() {
        BuildMode::None
    } else if idx.is_empty() {
        BuildMode::Sync
    } else {
        BuildMode::Background
    }
}
