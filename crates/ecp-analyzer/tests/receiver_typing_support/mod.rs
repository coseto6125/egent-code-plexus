//! Shared harness for the `<lang>_receiver_typing.rs` fixtures.
//!
//! Every fixture has the same shape: a base type declares `greet`, a derived
//! type in another file inherits it, an unrelated decoy type declares its own
//! `greet`, and a caller types a receiver as the derived type with the
//! language's existing binder. The bare name `greet` is ambiguous (base and
//! decoy), so only the receiver-typing ladder can produce the edge.
#![allow(dead_code)]

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

pub fn parse_all<P: LanguageProvider>(provider: &P, files: &[(&str, &str)]) -> Vec<LocalGraph> {
    files
        .iter()
        .map(|(path, src)| {
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .unwrap_or_else(|e| panic!("parse {path}: {e}"))
        })
        .collect()
}

/// Panics unless the binder attached `callee` to the node named `caller`.
/// A failure here is a binder gap, not a resolver gap.
pub fn assert_binder_emits(graphs: &[LocalGraph], caller: &str, callee: &str) {
    let calls: Vec<&str> = graphs
        .iter()
        .flat_map(|g| &g.nodes)
        .filter(|n| n.name == caller)
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect();
    assert!(
        calls.contains(&callee),
        "binder: `{caller}` should call `{callee}`; calls: {calls:?}"
    );
}

pub fn build(graphs: Vec<LocalGraph>) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for g in graphs {
        builder.add_graph(g);
    }
    builder.build()
}

/// Target file of every `Calls` edge from a node named `caller` to a node
/// named `callee`, one entry per edge.
pub fn callee_files(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            let file_idx = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file_idx].path.resolve(pool).to_string()
        })
        .collect()
}

/// The shared scenario: the binder types the receiver (`typed_callee`), and
/// the graph holds exactly one `Calls` edge from `caller` to `method`, into
/// `base_file` (not the decoy).
pub fn assert_single_call_into_base(
    graphs: Vec<LocalGraph>,
    caller: &str,
    typed_callee: &str,
    method: &str,
    base_file: &str,
) {
    assert_binder_emits(&graphs, caller, typed_callee);
    let graph = build(graphs);
    assert_eq!(
        callee_files(&graph, caller, method),
        vec![base_file.to_string()],
        "`{typed_callee}` must resolve to the inherited method in {base_file}, once"
    );
}
