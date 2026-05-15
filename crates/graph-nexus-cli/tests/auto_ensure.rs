use graph_nexus_cli::auto_ensure::{ensure_index, EnsureResult};
use std::fs;
use tempfile::TempDir;

#[test]
fn ensure_returns_ready_when_graph_exists_and_no_newer_source() {
    let tmp = TempDir::new().unwrap();
    // Write a source file FIRST (older mtime)
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();
    // Wait a moment, then write the graph (newer mtime)
    std::thread::sleep(std::time::Duration::from_millis(20));
    let graph_path = tmp.path().join("graph.bin");
    fs::write(&graph_path, vec![0u8; 16]).unwrap();

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(matches!(result, EnsureResult::Ready), "expected Ready, got {:?}", result);
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
    fs::write(&graph_path, vec![0u8; 16]).unwrap();
    // Wait, then touch a source file with newer mtime
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(matches!(result, EnsureResult::Stale { .. }), "expected Stale, got {:?}", result);
}
