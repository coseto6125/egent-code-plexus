//! `caller_count` / `callers` / `callee_count` / `callees` on a `find` hit
//! count `Calls` edges only.
//!
//! The raw in-degree also holds the `Defines` edge from the declaring file
//! and one `Imports` edge per file that pulls the symbol in. The hook renders
//! `callers` verbatim as `Called by:`, so a File node reached the model
//! labelled as a caller, and `caller_count` disagreed with
//! `ecp impact --direction upstream` on the same symbol.
//!
//! Two tests: the analyzer end-to-end proves the real pipeline emits those
//! `Defines` / `Imports` edges into a Function node, and the fixture test
//! pins the lists the hook renders, which the JSON payload does not carry.

mod common;

use common::{ecp_bin, init_and_analyze, write};

use ecp_cli::commands::find::{compute_hits, FindArgs, FindMode};
use ecp_cli::commands::hook::pre_tool_use::format_hits;
use ecp_cli::engine::Engine;
use ecp_cli::search::TantivyEngine;
use ecp_core::graph::{NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use serde_json::Value;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn find_caller_count_excludes_defines_and_imports_edges_end_to_end() {
    let tmp = tempdir().unwrap();
    let repo = tmp.path();

    // `target` has exactly one caller. Its declaring file adds a `Defines`
    // edge and `consumer.rs` adds an `Imports` edge without calling it, so
    // the raw in-degree is 3 while the true caller count is 1.
    write(
        repo,
        "src/lib.rs",
        r#"pub mod consumer;
pub fn target() {}
pub fn caller_a() { target(); }
"#,
    );
    write(
        repo,
        "src/consumer.rs",
        r#"use crate::target;
pub fn unrelated() { let _ = target; }
"#,
    );
    init_and_analyze(repo);

    let out = Command::new(ecp_bin())
        .args([
            "find", "target", "--mode", "bm25", "--format", "json", "--repo", ".",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp find failed to spawn");
    assert!(
        out.status.success(),
        "ecp find failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_slice(&out.stdout).expect("non-JSON output");
    let hit = result["source"]
        .as_array()
        .expect("source bucket")
        .iter()
        .find(|h| h["name"] == "target")
        .unwrap_or_else(|| panic!("no hit named target in {result}"));
    assert_eq!(hit["caller_count"], 1, "hit: {hit}");
}

#[test]
fn find_callers_and_callees_lists_hold_calls_edges_only() {
    let mut fx = GraphFixture::new();
    let lib = fx.node(NodeKind::File, "src/lib.rs", "lib.rs");
    let consumer = fx.node(NodeKind::File, "src/consumer.rs", "consumer.rs");
    let target = fx.func("src/lib.rs", "target");
    let caller_a = fx.func("src/lib.rs", "caller_a");
    let field = fx.node(NodeKind::Property, "src/lib.rs", "field");
    fx.edge_with(lib, target, RelType::Defines, 1.0, "defines");
    fx.edge_with(consumer, target, RelType::Imports, 1.0, "import");
    fx.edge_with(caller_a, target, RelType::Calls, 1.0, "call");
    fx.edge_with(target, field, RelType::ReadsField, 1.0, "read");
    let graph = fx.build();

    let dir = tempdir().unwrap();
    let bytes = rkyv::to_bytes::<Error>(&graph).expect("rkyv serialize");
    std::fs::write(dir.path().join("graph.bin"), bytes.as_slice()).unwrap();
    TantivyEngine::build_index(dir.path(), &graph).expect("tantivy build");
    let engine = Engine::load(dir.path().join("graph.bin")).expect("engine load");

    let args = FindArgs {
        pattern: Some("target".to_string()),
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
    let hit = hits
        .iter()
        .find(|h| h.name == "target")
        .expect("target hit must surface");

    assert_eq!(hit.caller_count, 1, "hit: {hit:?}");
    assert_eq!(hit.callers, vec!["caller_a".to_string()], "hit: {hit:?}");
    assert_eq!(hit.callee_count, 0, "ReadsField is not a call: {hit:?}");
    assert!(hit.callees.is_empty(), "hit: {hit:?}");

    // What the model reads: the file names never appear as callers.
    let rendered = format_hits(std::slice::from_ref(hit));
    assert!(
        rendered.contains("Called by: caller_a"),
        "rendered: {rendered}"
    );
    assert!(
        !rendered.contains("lib.rs,") && !rendered.contains("consumer.rs"),
        "rendered: {rendered}"
    );
    assert!(!rendered.contains("Calls:"), "rendered: {rendered}");
}
