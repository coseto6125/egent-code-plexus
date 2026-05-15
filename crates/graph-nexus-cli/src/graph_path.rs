//! Resolve the `--graph` arg to a concrete graph.bin path. When the user
//! passes the literal default, route through the registry for the current
//! repo/branch. Custom paths pass through unchanged.

use crate::git_state;
use std::path::{Path, PathBuf};

const LEGACY_DEFAULT: &str = ".gnx/graph.bin";

/// If `graph` matches the legacy default, replace with the registry-resolved
/// `~/.gnx/<repo>/<branch>/graph.bin` based on `cwd` (the repo root).
/// On any resolution failure (not a git repo, no registry entry, etc.) we
/// fall back to the original path — the caller's error handling will then
/// surface "graph.bin not found" with the old-style message.
pub fn resolve(graph: &Path, cwd: &Path) -> PathBuf {
    if graph.as_os_str() != LEGACY_DEFAULT {
        return graph.to_path_buf();
    }

    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let registry_path = home_gnx.join("registry.json");

    // Fast path: registry already lists this worktree → use the stored
    // index_dir directly. This is the common case after the first
    // `gnx admin index` and avoids recomputing the disambiguator-hash
    // layout, which can drift if the user moved the worktree.
    if let Ok(reg) = graph_nexus_core::registry::RegistryFile::read_or_empty(&registry_path) {
        if let Some((_repo, branch)) = reg.find_by_cwd(cwd, None) {
            return std::path::PathBuf::from(&branch.index_dir).join("graph.bin");
        }
    }

    // Cold path: first-run before indexing has populated the registry.
    // Compute the expected layout deterministically from git_state so the
    // CLI can write into the right place when `gnx admin index` runs.
    let state = match git_state::resolve(cwd) {
        Ok(s) => s,
        Err(_) => return graph.to_path_buf(),
    };
    let existing_repos: Vec<(String, String)> = {
        let reg = match graph_nexus_core::registry::Registry::open(&home_gnx) {
            Ok(r) => r,
            Err(_) => return graph.to_path_buf(),
        };
        reg.snapshot()
            .repos
            .iter()
            .map(|r| (r.name.clone(), r.worktree_path.clone()))
            .collect()
    };
    let layout = match graph_nexus_core::registry::IndexLayout::resolve(
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
