//! Resolve the `--graph` arg to a concrete `graph.bin` path.
//!
//! Custom absolute / non-default paths pass through unchanged. The legacy
//! default `.ecp/graph.bin` is routed through the v2 commit-content-addressed
//! layout: `<home>/.ecp/<repo>/commits/<dirname>/graph.bin`, resolved via
//! cwd's git common-dir + HEAD SHA + CommitIndex scan.

use crate::commit_lookup::CommitIndex;
use crate::git_cache;
use crate::repo_identity;
use ecp_core::registry::resolve_home_ecp;
use std::path::{Path, PathBuf};

const LEGACY_DEFAULT: &str = ".ecp/graph.bin";

pub fn resolve(graph: &Path, cwd: &Path) -> PathBuf {
    if is_custom(graph) {
        return graph.to_path_buf();
    }
    resolve_v2(cwd).unwrap_or_else(|| graph.to_path_buf())
}

/// True when `--graph` names an explicit path rather than the legacy default.
/// A custom path is taken literally: it must exist or the command errors —
/// it is never routed through v2 resolution or cwd warm-attach.
pub fn is_custom(graph: &Path) -> bool {
    graph.as_os_str() != LEGACY_DEFAULT
}

fn resolve_v2(cwd: &Path) -> Option<PathBuf> {
    let home_ecp = resolve_home_ecp();
    let repo_dir_name = repo_identity::repo_dir_name_for_cwd(cwd).ok()?;
    let commits = home_ecp.join(&repo_dir_name).join("commits");

    let head_sha = git_cache::head_sha_bytes(cwd)?;
    let idx = CommitIndex::scan_cached(&commits).ok()?;
    let dir = idx.find(&head_sha)?;
    Some(commits.join(dir).join("graph.bin"))
}

/// Process-cached `git rev-parse HEAD` parsed as 20 raw bytes. Kept as a
/// re-export so existing callers (`pre_tool_use`, etc.) don't churn imports.
pub(crate) fn head_sha_bytes(cwd: &Path) -> Option<[u8; 20]> {
    git_cache::head_sha_bytes(cwd)
}
