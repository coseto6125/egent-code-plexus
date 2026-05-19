use graph_nexus_cli::auto_ensure::{ensure_index, EnsureResult};
use graph_nexus_core::graph::{ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Materialise a minimal v3 graph.bin so `ensure_index`'s header pre-check
/// can confirm the file is well-formed and exit on mtime logic alone. The
/// graph carries no nodes / edges; only the magic + version fields matter
/// for these tests.
fn write_valid_empty_graph(path: &Path) {
    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: Vec::new(),
        files: Vec::new(),
        nodes: Vec::new(),
        edges: Vec::new(),
        out_offsets: vec![0],
        in_offsets: vec![0],
        in_edge_idx: Vec::new(),
        name_index: Vec::new(),
        process_start: 0,
        traces_offsets: Vec::new(),
        traces_data: Vec::new(),
        blind_spots: Vec::new(),
        route_shapes: Vec::new(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    fs::write(path, &*bytes).unwrap();
}

#[test]
fn ensure_returns_ready_when_graph_exists_and_no_newer_source() {
    let tmp = TempDir::new().unwrap();
    // Write a source file FIRST (older mtime)
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();
    // Wait a moment, then write the graph (newer mtime)
    std::thread::sleep(std::time::Duration::from_millis(20));
    let graph_path = tmp.path().join("graph.bin");
    write_valid_empty_graph(&graph_path);

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Ready),
        "expected Ready, got {:?}",
        result
    );
}

#[test]
fn ensure_reports_missing_when_graph_absent() {
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("nonexistent.bin");
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(matches!(result, EnsureResult::Missing));
}

#[test]
fn ensure_reports_stale_when_source_newer() {
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    write_valid_empty_graph(&graph_path);
    // Wait, then touch a source file with newer mtime
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Stale { .. }),
        "expected Stale, got {:?}",
        result
    );
}

/// `.gitignore`d dirs must not trip the staleness check, even when they
/// contain newer-than-graph files. Pre-fix: `auto_ensure` walked with
/// `.git_ignore(false)` and relied on a hardcoded `SKIP_DIRS` list, so a
/// repo whose `.gitignore` listed `.claude/` (sibling git worktrees) or
/// any other build/cache dir not in `SKIP_DIRS` would false-positive Stale
/// on every agent command, churning reindex.
#[test]
fn ensure_returns_ready_when_only_gitignored_files_are_newer() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(".gitignore"), ".claude/\nbuild_out/\n").unwrap();
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let graph_path = tmp.path().join("graph.bin");
    write_valid_empty_graph(&graph_path);

    // Newer files inside gitignored dirs — must NOT count as source changes.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::create_dir_all(tmp.path().join(".claude/worktrees/sibling")).unwrap();
    fs::write(
        tmp.path().join(".claude/worktrees/sibling/main.rs"),
        "fn other() {}",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("build_out")).unwrap();
    fs::write(tmp.path().join("build_out/gen.rs"), "fn gen() {}").unwrap();

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Ready),
        "expected Ready (gitignored dirs ignored), got {:?}",
        result
    );
}
