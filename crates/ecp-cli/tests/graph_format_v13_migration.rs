//! `graph.bin` v13 adds each file's import / export facts. A graph the
//! previous release wrote (v12) has none, and reading it as "no imports"
//! would make the overlay answer wrong without a trace: it must be rejected
//! and rebuilt. `fixtures/graph-written-by-0.14.8.bin` is the graph ecp
//! 0.14.8 built for the repo `setup_repo` writes minus `extra.py`, byte for
//! byte, not a fixture written by this code.

mod common;

use common::{ecp_bin, init_and_analyze, write, write_graph};
use ecp_cli::auto_ensure::{ensure_index, EnsureResult};
use ecp_cli::engine::{header_compatible, Engine};
use ecp_core::graph_fixture::GraphFixture;
use std::path::{Path, PathBuf};
use std::process::Command;

const GRAPH_WRITTEN_BY_0_14_8: &[u8] = include_bytes!("fixtures/graph-written-by-0.14.8.bin");

fn graph_bins(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut queue = vec![dir.to_path_buf()];
    while let Some(dir) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                queue.push(path);
            } else if path.file_name().is_some_and(|name| name == "graph.bin") {
                found.push(path);
            }
        }
    }
    found
}

fn sidecar(graph: &Path, suffix: &str) -> PathBuf {
    let mut path = graph.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

#[test]
fn test_header_compatible_graph_written_by_0_14_8_returns_false() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = write_graph(tmp.path(), GRAPH_WRITTEN_BY_0_14_8);
    assert!(
        !header_compatible(&graph),
        "a v12 graph passed the v13 header check"
    );
    let err = Engine::load(&graph)
        .err()
        .expect("a v12 graph must not load");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn test_ensure_index_graph_written_by_0_14_8_requires_full_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = write_graph(tmp.path(), GRAPH_WRITTEN_BY_0_14_8);
    match ensure_index(&graph, tmp.path()).unwrap() {
        EnsureResult::Stale {
            needs_full_rebuild, ..
        } => assert!(needs_full_rebuild, "a v12 graph took the overlay path"),
        other => panic!("a v12 graph must be stale, got {other:?}"),
    }
}

/// The facts are "missing" when their tables do not match the files, which
/// no build writes: such a graph is rejected like an old one. Tables that
/// record no import for a file are "legitimately empty" and load.
#[test]
fn test_header_compatible_missing_module_facts_false_empty_facts_true() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fx = GraphFixture::new();
    fx.func("pkg/core.py", "compute");
    let graph = fx.build();
    assert!(graph.imports.is_empty());
    let empty = write_graph(
        tmp.path(),
        &rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap(),
    );
    assert!(
        header_compatible(&empty),
        "recorded-but-empty facts must load"
    );

    let mut missing = graph;
    missing.file_imports.clear();
    let dir = tmp.path().join("missing");
    std::fs::create_dir_all(&dir).unwrap();
    let missing = write_graph(
        &dir,
        &rkyv::to_bytes::<rkyv::rancor::Error>(&missing).unwrap(),
    );
    assert!(
        !header_compatible(&missing),
        "absent facts must not read as no imports"
    );
}

/// End to end: the old graph sits where ecp 0.14.8 left it, sidecars and
/// all. A query must rebuild before it answers. `fresh_only` is in no file
/// the old graph knows, so finding it proves the answer came from a rebuild.
#[test]
fn test_find_on_graph_written_by_0_14_8_rebuilds_before_answering() {
    find_after_swapping_in_old_graph(Some("v0.14.8+schema5"));
}

/// A graph.bin that fails validation behind a meta.json naming the current
/// build (a torn write, a disk that filled mid-publish) must not be reused by
/// the rebuild the failed check asked for.
#[test]
fn test_find_on_unreadable_graph_with_current_meta_rebuilds_before_answering() {
    find_after_swapping_in_old_graph(None);
}

/// Swaps 0.14.8's graph into a freshly built slot, with the sidecars 0.14.8
/// writes, then asks for a symbol only the rebuilt graph has. `meta_fingerprint`
/// is what meta.json records: 0.14.8's own for a real upgrade, or `None` to
/// keep the current build's.
fn find_after_swapping_in_old_graph(meta_fingerprint: Option<&str>) {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    write(
        root,
        "app.py",
        "from pkg import compute\n\n\ndef go():\n    return compute()\n",
    );
    write(root, "pkg/__init__.py", "from .core import compute\n");
    write(root, "pkg/core.py", "def compute():\n    return 1\n");
    write(root, "extra.py", "def fresh_only():\n    return 2\n");
    init_and_analyze(root);
    let graphs = graph_bins(&root.join(".ecp"));
    assert_eq!(graphs.len(), 1, "{graphs:?}");
    let graph = &graphs[0];
    std::fs::write(graph, GRAPH_WRITTEN_BY_0_14_8).unwrap();
    std::fs::write(sidecar(graph, ".compatible_version"), "12\n").unwrap();
    std::fs::write(sidecar(graph, ".builder_fingerprint"), "v0.14.8+schema5\n").unwrap();
    if let Some(fingerprint) = meta_fingerprint {
        let meta_path = graph.with_file_name("meta.json");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
        meta["builder_fingerprint"] = fingerprint.into();
        std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    }
    assert!(!header_compatible(graph));

    let out = Command::new(ecp_bin())
        .args(["find", "fresh_only"])
        .current_dir(root)
        .env("HOME", root)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("extra.py"),
        "the query answered from the old graph: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        graph_bins(&root.join(".ecp"))
            .iter()
            .any(|graph| header_compatible(graph)),
        "no v13 graph was built"
    );
}
