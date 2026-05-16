//! `auto_ensure::ensure_index` must treat graph.bin files that fail
//! magic / version / structural validation as Stale, so `ensure_fresh`
//! triggers a clean rebuild rather than surfacing
//! `engine::Engine::load`'s InvalidData error to agent commands the
//! next time the user upgrades the CLI past a GRAPH_FORMAT_VERSION bump.

use graph_nexus_cli::auto_ensure::{ensure_index, EnsureResult};
use std::fs;
use tempfile::tempdir;

#[test]
fn ensure_index_returns_stale_for_corrupt_graph() {
    let dir = tempdir().unwrap();
    let worktree = dir.path();
    let graph_path = worktree.join("graph.bin");
    fs::write(&graph_path, [0xFFu8; 64]).unwrap();

    let result = ensure_index(&graph_path, worktree).unwrap();
    assert!(
        matches!(result, EnsureResult::Stale { .. }),
        "expected Stale for corrupt graph, got {result:?}"
    );
}

#[test]
fn ensure_index_returns_stale_for_truncated_graph() {
    let dir = tempdir().unwrap();
    let worktree = dir.path();
    let graph_path = worktree.join("graph.bin");
    fs::write(&graph_path, [0u8; 4]).unwrap();

    let result = ensure_index(&graph_path, worktree).unwrap();
    assert!(
        matches!(result, EnsureResult::Stale { .. }),
        "expected Stale for truncated graph, got {result:?}"
    );
}

#[test]
fn ensure_index_returns_missing_when_absent() {
    let dir = tempdir().unwrap();
    let worktree = dir.path();
    let graph_path = worktree.join("graph.bin");

    let result = ensure_index(&graph_path, worktree).unwrap();
    assert!(
        matches!(result, EnsureResult::Missing),
        "expected Missing when graph.bin absent, got {result:?}"
    );
}
