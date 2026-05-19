//! Resolve the `--graph` arg to a concrete `graph.bin` path.
//!
//! Custom absolute / non-default paths pass through unchanged. The legacy
//! default `.gnx/graph.bin` is routed through the v2 commit-content-addressed
//! layout: `<home>/.gnx/<repo>/commits/<dirname>/graph.bin`, resolved via
//! cwd's git common-dir + HEAD SHA + CommitIndex scan.

use crate::commit_lookup::CommitIndex;
use crate::git::safe_exec;
use crate::repo_identity;
use cgn_core::registry::resolve_home_gnx;
use std::path::{Path, PathBuf};

const LEGACY_DEFAULT: &str = ".gnx/graph.bin";

pub fn resolve(graph: &Path, cwd: &Path) -> PathBuf {
    if graph.as_os_str() != LEGACY_DEFAULT {
        return graph.to_path_buf();
    }
    resolve_v2(cwd).unwrap_or_else(|| graph.to_path_buf())
}

fn resolve_v2(cwd: &Path) -> Option<PathBuf> {
    let home_gnx = resolve_home_gnx();
    let repo_dir_name = repo_identity::repo_dir_name_for_cwd(cwd).ok()?;
    let commits = home_gnx.join(&repo_dir_name).join("commits");

    let head_sha = head_sha_bytes(cwd)?;
    let idx = CommitIndex::scan(&commits).ok()?;
    let dir = idx.find(&head_sha)?;
    Some(commits.join(dir).join("graph.bin"))
}

pub(crate) fn head_sha_bytes(cwd: &Path) -> Option<[u8; 20]> {
    let out = safe_exec::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = std::str::from_utf8(&out.stdout).ok()?.trim();
    if s.len() != 40 {
        return None;
    }
    let mut sha = [0u8; 20];
    hex::decode_to_slice(s, &mut sha).ok()?;
    Some(sha)
}
