//! Shared harness for `constant_reader_edges.rs` and
//! `callback_argument_edges.rs`: a bare name a function body uses as a value
//! (`LIMITS`, `handler`) binds the declaration it names.
//!
//! Every helper reads the graph the real parse + `GraphBuilder` pipeline
//! built; nothing here re-implements resolution.
#![allow(dead_code)]

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

pub const VALUE_READ: &str = "value_read";
pub const FUNCTION_REFERENCE: &str = "function_reference";

/// One edge out of a reader: `(target file, target name, reason, confidence)`.
pub type Ref = (String, String, String, f32);

pub fn graph(provider: &dyn LanguageProvider, files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        let local = provider
            .parse_file(Path::new(path), src.as_bytes())
            .unwrap_or_else(|e| panic!("parse {path}: {e}"));
        builder.add_graph(local);
    }
    builder.build()
}

/// Every `rel` edge whose source is named `source` and whose reason is
/// `reason`, sorted.
pub fn refs(graph: &ZeroCopyGraph, source: &str, rel: RelType, reason: &str) -> Vec<Ref> {
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<Ref> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == rel && e.reason.resolve(pool) == reason)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == source)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            let file = if target.has_owning_file() {
                graph.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string()
            } else {
                String::new()
            };
            (
                file,
                target.name.resolve(pool).to_string(),
                reason.to_string(),
                e.confidence,
            )
        })
        .collect();
    out.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    out
}

/// The `value_read` edges (`ReadsField`) out of `reader`.
pub fn value_reads(graph: &ZeroCopyGraph, reader: &str) -> Vec<Ref> {
    refs(graph, reader, RelType::ReadsField, VALUE_READ)
}

/// The `function_reference` edges (`References`) out of `registrar`.
pub fn function_refs(graph: &ZeroCopyGraph, registrar: &str) -> Vec<Ref> {
    refs(graph, registrar, RelType::References, FUNCTION_REFERENCE)
}

/// The `Calls` targets of `caller` as `(file, name)`, sorted.
pub fn calls(graph: &ZeroCopyGraph, caller: &str) -> Vec<(String, String)> {
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<(String, String)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            let file = if target.has_owning_file() {
                graph.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string()
            } else {
                String::new()
            };
            (file, target.name.resolve(pool).to_string())
        })
        .collect();
    out.sort();
    out
}

/// `(file, name, reason, confidence)` as the tests write an expected edge.
pub fn r(file: &str, name: &str, reason: &str, confidence: f32) -> Ref {
    (
        file.to_string(),
        name.to_string(),
        reason.to_string(),
        confidence,
    )
}

/// Same-file tier.
pub const SAME_FILE: f32 = 1.0;
/// Import tier.
pub const IMPORT: f32 = 0.95;
/// Unique project-wide declaration: the lowest confidence a name binds at.
pub const GLOBAL: f32 = 0.7;

/// The one edge in `edges` names `(file, name)` at a confidence of at least
/// [`GLOBAL`]: for a cross-file read whose tier (import or global) depends
/// on how the language's import is recorded.
pub fn assert_one_cross_file(edges: &[Ref], file: &str, name: &str) {
    assert_eq!(edges.len(), 1, "one edge expected: {edges:?}");
    let (f, n, _, confidence) = &edges[0];
    assert_eq!((f.as_str(), n.as_str()), (file, name), "{edges:?}");
    assert!(
        *confidence >= GLOBAL,
        "never below the global tier: {edges:?}"
    );
}
