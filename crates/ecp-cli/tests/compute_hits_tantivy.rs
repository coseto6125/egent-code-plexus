//! Regression: `compute_hits` must route through the existing
//! `TantivyEngine` index, not the hardcoded 1.0 / 0.7 / 0.4 substring
//! scoring in `bm25_hits_from_graph`. Drives B+ step 1 (tantivy wireup).

use ecp_cli::commands::find::{compute_hits, FindArgs, FindMode};
use ecp_cli::engine::Engine;
use ecp_cli::search::TantivyEngine;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use ecp_core::graph_fixture::GraphFixture;
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
    let mut fx = GraphFixture::new();
    let ids: Vec<u32> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let id = fx.func("src/config.rs", name);
            fx.span(id, (i as u32, 0, i as u32 + 1, 0));
            id
        })
        .collect();

    // Edges: loadSettings -> parseConfig, initApp -> parseConfig,
    //        parseConfig -> tokenize.
    let parse_config_idx = ids[0];
    let load_settings_idx = ids[3];
    let init_app_idx = ids[4];
    let tokenize_idx = ids[5];
    fx.edge_with(parse_config_idx, tokenize_idx, RelType::Calls, 1.0, "call");
    fx.edge_with(
        load_settings_idx,
        parse_config_idx,
        RelType::Calls,
        1.0,
        "call",
    );
    fx.edge_with(init_app_idx, parse_config_idx, RelType::Calls, 1.0, "call");

    fx.build()
}

/// Persist graph.bin into `<index_dir>/graph.bin`. The tempdir itself
/// stands in for `~/.ecp/<repo>/<branch>/` — tantivy and meta.json sit
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

    let args = FindArgs {
        pattern: Some("config".to_string()),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        file: None,
        repo: None,
        format: None,
        batch: false,
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

    let args = FindArgs {
        pattern: Some("parseConfig".to_string()),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        file: None,
        repo: None,
        format: None,
        batch: false,
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
    // Each listed name carries the file it is declared in, which the hook
    // proves before it names the caller or callee.
    assert_eq!(parse_config.caller_files, vec!["src/config.rs"; 2]);
    assert_eq!(parse_config.callee_files, vec!["src/config.rs"]);
}

/// One caller holding 4 call sites of `hubTarget`, and `hubTarget` calling
/// `leafCallee` from 4 sites: one `Calls` edge per site.
fn make_multi_site_graph() -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    let ids: Vec<u32> = ["hubTarget", "multiSiteCaller", "leafCallee"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let id = fx.func("src/hub.rs", name);
            fx.span(id, (i as u32 * 10, 0, i as u32 * 10 + 9, 0));
            id
        })
        .collect();
    let (hub, caller, leaf) = (ids[0], ids[1], ids[2]);
    for _ in 0..4 {
        fx.edge_with(caller, hub, RelType::Calls, 1.0, "call");
        fx.edge_with(hub, leaf, RelType::Calls, 1.0, "call");
    }
    fx.build()
}

/// Stands for: `Called by:` / `Calls:` and their counts name distinct
/// symbols, not call sites. Before the fix the hit carried
/// `callers == ["multiSiteCaller"; 3]`, `caller_count == 4`, and rendered
/// `Called by: multiSiteCaller, multiSiteCaller, multiSiteCaller, +1 more`.
#[test]
fn compute_hits_one_caller_many_call_sites_counts_once() {
    let dir = tempdir().unwrap();
    let index_dir = dir.path();
    let graph = make_multi_site_graph();
    persist_graph(index_dir, &graph);
    TantivyEngine::build_index(index_dir, &graph).expect("tantivy build");
    let engine = Engine::load(index_dir.join("graph.bin")).expect("engine load");

    let args = FindArgs {
        pattern: Some("hubTarget".to_string()),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        file: None,
        repo: None,
        format: None,
        batch: false,
    };
    let hits = compute_hits(args, &engine).expect("compute_hits");
    let hub = hits
        .iter()
        .find(|h| h.name == "hubTarget")
        .expect("hubTarget hit must surface");
    assert_eq!(
        (hub.caller_count, hub.callers.clone()),
        (1, vec!["multiSiteCaller".to_string()]),
        "{hub:?}"
    );
    assert_eq!(
        (hub.callee_count, hub.callees.clone()),
        (1, vec!["leafCallee".to_string()]),
        "{hub:?}"
    );
}
