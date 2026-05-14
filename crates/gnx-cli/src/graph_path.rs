//! Resolve the `--graph` arg to a concrete graph.bin path. When the user
//! passes the literal default, route through the registry for the current
//! repo/branch. Custom paths pass through unchanged.

use crate::git_state;
use std::path::{Path, PathBuf};

const LEGACY_DEFAULT: &str = ".gitnexus-rs/graph.bin";

/// If `graph` matches the legacy default, replace with the registry-resolved
/// `~/.gnx/<repo>/<branch>/graph.bin` based on `cwd` (the repo root).
/// On any resolution failure (not a git repo, no registry entry, etc.) we
/// fall back to the original path — the caller's error handling will then
/// surface "graph.bin not found" with the old-style message.
pub fn resolve(graph: &Path, cwd: &Path) -> PathBuf {
    if graph.as_os_str() != LEGACY_DEFAULT {
        return graph.to_path_buf();
    }

    let state = match git_state::resolve(cwd) {
        Ok(s) => s,
        Err(_) => return graph.to_path_buf(),
    };
    let home_gnx = gnx_core::registry::resolve_home_gnx();

    let existing_repos: Vec<(String, String)> = {
        let reg = match gnx_core::registry::Registry::open(&home_gnx) {
            Ok(r) => r,
            Err(_) => return graph.to_path_buf(),
        };
        reg.snapshot()
            .repos
            .iter()
            .map(|r| (r.name.clone(), r.worktree_path.clone()))
            .collect()
    };
    let layout = match gnx_core::registry::IndexLayout::resolve(
        &home_gnx,
        &state.repo_name,
        &state.branch,
        state.worktree_path.to_string_lossy().as_ref(),
        &existing_repos,
    ) {
        Ok(l) => l,
        Err(_) => return graph.to_path_buf(),
    };
    layout.index_dir.join("graph.bin")
}
