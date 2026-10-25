//! Shared helpers for the `overrides_<lang>` and `<lang>_overrides_heritage`
//! tests. The `Overrides` pass reads the supertypes that Pass 1.9 resolves,
//! so every helper builds the whole graph through `GraphBuilder`.
//!
//! Each integration-test crate compiles its own copy of this module, so some
//! callers won't use every helper — `allow(dead_code)` keeps the warning
//! quiet across all dialects.
#![allow(dead_code)]

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::contracts::raw_records::Evidence as StoredEvidence;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

pub fn build(local_graphs: &[LocalGraph]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for lg in local_graphs {
        builder.add_graph(lg.clone());
    }
    builder.build()
}

/// `(source, target)` node indices of every `Overrides` edge of the built
/// graph.
pub fn run_overrides(local_graphs: &[LocalGraph]) -> Vec<(u32, u32)> {
    let graph = build(local_graphs);
    graph
        .edges
        .iter()
        .filter(|e| matches!(e.rel_type, RelType::Overrides))
        .map(|e| (e.source, e.target))
        .collect()
}

pub fn has_decorator(node: &RawNode, d: &str) -> bool {
    node.decorators.iter().any(|dec| dec.contains(d))
}

/// The graph of `files`, each `(path, source)` parsed by `provider`.
pub fn graph_of(provider: &dyn LanguageProvider, files: &[(&str, &str)]) -> ZeroCopyGraph {
    let graphs: Vec<LocalGraph> = files
        .iter()
        .map(|(path, src)| {
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .expect("parse")
        })
        .collect();
    build(&graphs)
}

/// One endpoint of an edge as a reader of `ecp impact` sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct End {
    pub file: String,
    pub owner: String,
    pub name: String,
    /// 0-based start row.
    pub line: u32,
}

fn end(graph: &ZeroCopyGraph, idx: u32) -> End {
    let pool = graph.string_pool.as_slice();
    let node = &graph.nodes[idx as usize];
    End {
        file: graph.files[node.file_idx as usize]
            .path
            .resolve(pool)
            .to_string(),
        owner: node.owner_class.resolve(pool).to_string(),
        name: node.name.resolve(pool).to_string(),
        line: node.span.0,
    }
}

/// `(source, target, confidence, reason)` of every `Overrides` edge.
pub fn overrides(graph: &ZeroCopyGraph) -> Vec<(End, End, f32, String)> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Overrides)
        .map(|e| {
            (
                end(graph, e.source),
                end(graph, e.target),
                e.confidence,
                e.reason.resolve(pool).to_string(),
            )
        })
        .collect()
}

/// `(file, 0-based start row, hint)` of every BlindSpot of `kind`.
pub fn blind_spots(graph: &ZeroCopyGraph, kind: &str) -> Vec<(String, u32, String)> {
    let pool = graph.string_pool.as_slice();
    graph
        .blind_spots
        .iter()
        .filter(|b| b.kind.resolve(pool) == kind)
        .map(|b| {
            (
                b.file_path.resolve(pool).to_string(),
                b.start_row,
                b.hint.resolve(pool).to_string(),
            )
        })
        .collect()
}

/// How the pass pairs an override with its base in a language (the rule
/// table of `post_process::overrides`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// A parser marker or a Rust `impl Trait for T` block: confidence 1.0.
    Exact,
    /// A name match in the declared base: below the 0.7 ceiling.
    Heuristic,
}

/// The fixture of every `<lang>_overrides_heritage` test: `decoy` and
/// `child` both declare a type `Base` with a method `method`; `child` also
/// declares `Child`, whose declared base is its own file's `Base`, with an
/// overriding `method`. `decoy` sorts first, so a pass that pairs by type
/// name alone reaches the decoy's method.
///
/// Asserts the one `Overrides` edge a reader gets: from `child`'s
/// overriding `method` to the other `method` of `child` (its `Base`'s),
/// never into `decoy`, with the confidence class of `evidence`. The
/// endpoints are told apart by file and name, not by `owner_class`, which
/// not every parser stamps.
pub fn assert_binds_declared_base(
    provider: &dyn LanguageProvider,
    decoy: (&str, &str),
    child: (&str, &str),
    method: &str,
    evidence: Evidence,
) {
    let graph = graph_of(provider, &[decoy, child]);
    let edges = overrides(&graph);
    assert_eq!(
        edges.len(),
        1,
        "{}: one Overrides edge, Child.{method} -> Base.{method}; got {edges:?}",
        child.0
    );
    let (source, target, confidence, reason) = &edges[0];
    for (role, end) in [("source", source), ("target", target)] {
        assert_eq!(
            (end.file.as_str(), end.name.as_str()),
            (child.0, method),
            "{}: the {role} is a `{method}` of the child file, never the decoy's; got {edges:?}",
            child.0
        );
    }
    assert!(
        target.line < source.line,
        "{}: the target is Base's `{method}`, declared before Child's; got {edges:?}",
        child.0
    );
    // A machine reader classifies a stored edge by its band
    // (`Evidence::of_stored`), and tells the producer's rules apart by
    // `reason`; both are the contract, not the exact float.
    let (stored, expected_reason) = match evidence {
        Evidence::Exact => (StoredEvidence::Exact, "post_process:overrides"),
        Evidence::Heuristic => (
            StoredEvidence::Heuristic,
            "post_process:overrides:name-in-base",
        ),
    };
    assert_eq!(
        StoredEvidence::of_stored(*confidence, RelType::Overrides),
        stored,
        "{}: confidence {confidence}",
        child.0
    );
    assert_eq!(reason, expected_reason, "{}", child.0);
}
