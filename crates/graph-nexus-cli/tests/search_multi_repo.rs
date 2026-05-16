//! Multi-repo fan-out: verifies the pre-loaded-engine path
//! (`compute_multi_with_engines`) returns merged hits from every repo
//! and degrades gracefully when one engine fails to load.

use graph_nexus_cli::commands::search::{
    compute_multi_with_engines, load_engines_lossy, SearchMode,
};
use graph_nexus_cli::engine::Engine;
use graph_nexus_core::graph::{
    File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use rkyv::rancor::Error;
use std::fs;
use tempfile::TempDir;

/// Build a graph with the given function names — one file, one symbol per name.
/// All names share the substring "shared" so the BM25 path returns hits from
/// every repo and the fan-out merger has something to combine.
fn build_graph(names: &[&str]) -> ZeroCopyGraph {
    let mut pool = StringPool::new();
    let file_path_ref = pool.add("src/lib.rs");
    let nodes: Vec<Node> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let name_ref = pool.add(name);
            let uid_ref = pool.add(&format!("Function:src/lib.rs:{name}"));
            Node {
                uid: uid_ref,
                name: name_ref,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (i as u32, 0, i as u32 + 1, 0),
                community_id: 0,
            }
        })
        .collect();
    let n = nodes.len();
    let out_offsets: Vec<u32> = (0..=n as u32).map(|_| 0).collect();
    let in_offsets: Vec<u32> = (0..=n as u32).map(|_| 0).collect();
    ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: file_path_ref,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Source,
        }],
        nodes,
        edges: vec![],
        out_offsets,
        in_offsets,
        in_edge_idx: vec![],
        name_index: vec![],
        process_start: n as u32,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    }
}

fn persist_graph(dir: &std::path::Path, graph: &ZeroCopyGraph) {
    fs::create_dir_all(dir).unwrap();
    let bytes = rkyv::to_bytes::<Error>(graph).expect("rkyv serialize");
    fs::write(dir.join("graph.bin"), bytes.as_slice()).expect("write graph.bin");
}

#[test]
fn compute_multi_with_engines_merges_hits_from_both_repos() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    persist_graph(dir_a.path(), &build_graph(&["alpha_shared", "alpha_only"]));
    persist_graph(dir_b.path(), &build_graph(&["beta_shared", "beta_only"]));

    let loaded = vec![
        (
            "alpha".to_string(),
            Engine::load(dir_a.path().join("graph.bin")).map_err(|e| e.to_string()),
        ),
        (
            "beta".to_string(),
            Engine::load(dir_b.path().join("graph.bin")).map_err(|e| e.to_string()),
        ),
    ];

    let (hits, summary) = compute_multi_with_engines("shared", &SearchMode::Bm25, None, &loaded);

    let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    assert!(
        names.contains(&"alpha_shared"),
        "alpha_shared missing in {names:?}"
    );
    assert!(
        names.contains(&"beta_shared"),
        "beta_shared missing in {names:?}"
    );
    assert!(
        hits.iter().any(|h| h.repo.as_deref() == Some("alpha")),
        "no hit tagged repo=alpha"
    );
    assert!(
        hits.iter().any(|h| h.repo.as_deref() == Some("beta")),
        "no hit tagged repo=beta"
    );
    assert!(
        summary.contains("2 repo(s) targeted") && summary.contains("0 failed"),
        "unexpected summary: {summary}"
    );
}

#[test]
fn compute_multi_with_engines_tolerates_failed_engine_load() {
    let dir_a = TempDir::new().unwrap();
    persist_graph(dir_a.path(), &build_graph(&["alpha_shared"]));

    let loaded = vec![
        (
            "alpha".to_string(),
            Engine::load(dir_a.path().join("graph.bin")).map_err(|e| e.to_string()),
        ),
        (
            "broken".to_string(),
            Err::<Engine, String>("simulated load failure".to_string()),
        ),
    ];

    let (hits, summary) = compute_multi_with_engines("shared", &SearchMode::Bm25, None, &loaded);

    assert!(
        hits.iter().any(|h| h.name == "alpha_shared"),
        "alpha_shared missing despite alpha engine being healthy: {hits:?}"
    );
    assert!(
        summary.contains("1 failed"),
        "expected 1 failed in summary, got: {summary}"
    );
}

#[test]
fn load_engines_lossy_captures_per_repo_failures() {
    let dir_a = TempDir::new().unwrap();
    persist_graph(dir_a.path(), &build_graph(&["alpha_only"]));

    let targets = vec![
        (
            "alpha".to_string(),
            dir_a
                .path()
                .join("graph.bin")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "missing".to_string(),
            "/nonexistent/path/graph.bin".to_string(),
        ),
    ];

    let loaded = load_engines_lossy(&targets);
    assert_eq!(loaded.len(), 2);
    assert!(
        loaded[0].1.is_ok(),
        "alpha should load (err: {:?})",
        loaded[0].1.as_ref().err()
    );
    assert!(loaded[1].1.is_err(), "missing path should error");
}
