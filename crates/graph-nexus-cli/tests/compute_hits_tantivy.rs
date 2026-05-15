//! Regression: `compute_hits` must route through the existing
//! `TantivyEngine` index, not the hardcoded 1.0 / 0.7 / 0.4 substring
//! scoring in `bm25_hits_from_graph`. Drives B+ step 1 (tantivy wireup).

use graph_nexus_cli::commands::hook::pre_tool_use::format_hits;
use graph_nexus_cli::commands::search::{compute_hits, Hit, SearchArgs, SearchMode};
use graph_nexus_cli::engine::Engine;
use graph_nexus_cli::search::TantivyEngine;
use graph_nexus_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

/// Build a graph with names that exercise tokenization. Substring scan
/// returns identical scores for any name containing "config"; tantivy
/// gives distinct BM25 scores based on term frequency and field length.
fn make_config_graph() -> ZeroCopyGraph {
    let names = [
        "parseConfig",       // exact-ish — short name, single term match
        "configParser",      // also single-term
        "parse_config_file", // longer — lower BM25 due to field length norm
        "loadSettings",      // caller of parseConfig
        "initApp",           // caller of parseConfig
        "tokenize",          // callee of parseConfig
    ];
    let mut pool = StringPool::new();
    let file_path_ref = pool.add("src/config.rs");
    let nodes: Vec<Node> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let name_ref = pool.add(name);
            let uid_ref = pool.add(&format!("Function:src/config.rs:{name}"));
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

    // Edges: loadSettings -> parseConfig, initApp -> parseConfig,
    //        parseConfig -> tokenize. Sorted by source for CSR.
    let parse_config_idx = 0u32;
    let load_settings_idx = 3u32;
    let init_app_idx = 4u32;
    let tokenize_idx = 5u32;
    let reason_ref = pool.add("call");
    let edges = vec![
        Edge {
            source: parse_config_idx,
            target: tokenize_idx,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason: reason_ref,
        },
        Edge {
            source: load_settings_idx,
            target: parse_config_idx,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason: reason_ref,
        },
        Edge {
            source: init_app_idx,
            target: parse_config_idx,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason: reason_ref,
        },
    ];
    // out_offsets[i..i+1] slices into edges; nodes sorted as above.
    // node 0 (parseConfig): edges[0..1] -> 1 outgoing (tokenize)
    // node 1 (configParser): 0 outgoing
    // node 2 (parse_config_file): 0 outgoing
    // node 3 (loadSettings): edges[1..2] -> 1 outgoing (parseConfig)
    // node 4 (initApp): edges[2..3] -> 1 outgoing (parseConfig)
    // node 5 (tokenize): 0 outgoing
    let out_offsets = vec![0u32, 1, 1, 1, 2, 3, 3];

    // in_edge_idx + in_offsets — incoming for each node by indexing into edges.
    // parseConfig (0): edges[1], edges[2] incoming
    // configParser (1): none
    // parse_config_file (2): none
    // loadSettings (3): none
    // initApp (4): none
    // tokenize (5): edges[0] incoming
    let in_edge_idx = vec![1u32, 2, 0];
    let in_offsets = vec![0u32, 2, 2, 2, 2, 2, 3];

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
        edges,
        out_offsets,
        in_offsets,
        in_edge_idx,
        name_index: vec![],
        embeddings: None,
        process_start: 6,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    }
}

/// Persist graph.bin into `<index_dir>/graph.bin`. The tempdir itself
/// stands in for `~/.gnx/<repo>/<branch>/` — tantivy and meta.json sit
/// alongside it in the same dir.
fn persist_graph(index_dir: &std::path::Path, graph: &ZeroCopyGraph) {
    fs::create_dir_all(index_dir).unwrap();
    let bytes = rkyv::to_bytes::<Error>(graph).expect("rkyv serialize");
    fs::write(index_dir.join("graph.bin"), bytes.as_slice()).expect("write graph.bin");
}

#[test]
fn compute_hits_uses_tantivy_not_substring_scoring() {
    let dir = tempdir().unwrap();
    let index_dir = dir.path();
    let graph = make_config_graph();
    persist_graph(index_dir, &graph);
    TantivyEngine::build_index(index_dir, &graph).expect("tantivy build");

    let engine = Engine::load(index_dir.join("graph.bin")).expect("engine load");

    let args = SearchArgs {
        pattern: "config".to_string(),
        mode: SearchMode::Bm25,
        kind: None,
        repo: None,
        format: None,
    };
    let hits = compute_hits(args, &engine).expect("compute_hits");

    assert!(!hits.is_empty(), "expected hits for 'config', got none");

    // Substring scan would give exactly 0.4 (substring) or 0.7 (prefix)
    // — both hardcoded. Tantivy BM25 gives floating scores depending on
    // tf/idf/field length. If any hit's score is one of the hardcoded
    // values, the substring path is still wired.
    for h in &hits {
        assert!(
            h.score != 1.0 && h.score != 0.7 && h.score != 0.4,
            "hit '{}' scored {} — matches the hardcoded substring-scan values, \
             meaning tantivy is not wired",
            h.name,
            h.score
        );
    }

    // All three config-named symbols should surface.
    let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    assert!(
        names.contains(&"parseConfig"),
        "missing parseConfig in {names:?}"
    );
    assert!(
        names.contains(&"configParser"),
        "missing configParser in {names:?}"
    );
    assert!(
        names.contains(&"parse_config_file"),
        "missing parse_config_file in {names:?}"
    );
}

#[test]
fn compute_hits_populates_one_hop_callers_and_callees() {
    let dir = tempdir().unwrap();
    let index_dir = dir.path();
    let graph = make_config_graph();
    persist_graph(index_dir, &graph);
    TantivyEngine::build_index(index_dir, &graph).expect("tantivy build");
    let engine = Engine::load(index_dir.join("graph.bin")).expect("engine load");

    let args = SearchArgs {
        pattern: "parseConfig".to_string(),
        mode: SearchMode::Bm25,
        kind: None,
        repo: None,
        format: None,
    };
    let hits = compute_hits(args, &engine).expect("compute_hits");

    let parse_config = hits
        .iter()
        .find(|h| h.name == "parseConfig")
        .expect("parseConfig hit must surface");

    // Fixture wires loadSettings → parseConfig and initApp → parseConfig.
    let mut callers = parse_config.callers.clone();
    callers.sort();
    assert_eq!(
        callers,
        vec!["initApp".to_string(), "loadSettings".to_string()],
        "callers should be drawn from in_edges via CSR"
    );

    // Fixture wires parseConfig → tokenize.
    assert_eq!(
        parse_config.callees,
        vec!["tokenize".to_string()],
        "callees should be drawn from out_edges via CSR"
    );
}

#[test]
fn format_hits_emits_legacy_style_called_by_and_calls_block() {
    // Build a Hit by hand — no graph plumbing needed for the formatter.
    let hit = Hit {
        repo: None,
        score: 1.23,
        kind: "Function".to_string(),
        file: "src/config.rs".to_string(),
        line: 42,
        name: "parseConfig".to_string(),
        signature: "Function parseConfig".to_string(),
        caller_count: 2,
        callers: vec!["loadSettings".to_string(), "initApp".to_string()],
        callees: vec!["tokenize".to_string()],
    };
    let out = format_hits(&[hit]);
    assert!(out.contains("parseConfig (src/config.rs:42)"), "got: {out}");
    assert!(out.contains("[Function]"), "kind tag missing: {out}");
    assert!(
        out.contains("Called by: loadSettings, initApp"),
        "callers line missing: {out}"
    );
    assert!(
        out.contains("Calls: tokenize"),
        "callees line missing: {out}"
    );
}

#[test]
fn format_hits_skips_empty_caller_callee_lines() {
    let hit = Hit {
        repo: None,
        score: 0.5,
        kind: "Function".to_string(),
        file: "src/main.rs".to_string(),
        line: 1,
        name: "orphan".to_string(),
        signature: "Function orphan".to_string(),
        caller_count: 0,
        callers: vec![],
        callees: vec![],
    };
    let out = format_hits(&[hit]);
    assert!(out.contains("orphan (src/main.rs:1)"));
    assert!(!out.contains("Called by:"), "empty callers must be skipped");
    assert!(!out.contains("Calls:"), "empty callees must be skipped");
}
