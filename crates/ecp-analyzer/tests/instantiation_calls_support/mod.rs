//! Shared harness for the `<lang>_instantiation_calls.rs` and
//! `<lang>_import_bound_calls.rs` fixtures.
//!
//! An instantiation (`new A()`, `A()`, `A.new`) must give `ecp impact` a
//! caller for `A`. The contract: one `Calls` edge from the instantiating
//! function to `A`'s single declared constructor, or to the type node itself
//! when `A` declares no constructor or several. A same-named callable keeps
//! winning, and one call site never produces two edges.
//!
//! Every helper reads the graph that the real parse + `GraphBuilder` pipeline
//! built; nothing here re-implements resolution.
#![allow(dead_code)]

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

/// One edge target, flattened for assertion messages.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub name: String,
    pub kind: NodeKind,
    pub owner: String,
    pub file: String,
    pub confidence: f32,
}

pub fn graph_of<P: LanguageProvider>(provider: &P, files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        let local = provider
            .parse_file(Path::new(path), src.as_bytes())
            .unwrap_or_else(|e| panic!("parse {path}: {e}"));
        builder.add_graph(local);
    }
    builder.build()
}

/// The `Calls` edges from `caller` are exactly `expected`, each given as
/// `(file, owner, name, confidence)`, in edge order. The import tier is
/// told apart from the global tier only by the confidence.
pub fn assert_call_targets(
    graph: &ZeroCopyGraph,
    caller: &str,
    expected: &[(&str, &str, &str, f32)],
) {
    let hits = calls_from(graph, caller);
    let actual: Vec<_> = hits
        .iter()
        .map(|h| {
            (
                h.file.as_str(),
                h.owner.as_str(),
                h.name.as_str(),
                h.confidence,
            )
        })
        .collect();
    assert_eq!(actual, expected, "`{caller}` Calls edges: {hits:?}");
}

/// Every `rel` edge whose source node is named `caller`, one entry per edge.
pub fn edges_from(graph: &ZeroCopyGraph, caller: &str, rel: RelType) -> Vec<Hit> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == rel)
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
            Hit {
                name: target.name.resolve(pool).to_string(),
                kind: target.kind,
                owner: target.owner_class.resolve(pool).to_string(),
                file,
                confidence: e.confidence,
            }
        })
        .collect()
}

pub fn calls_from(graph: &ZeroCopyGraph, caller: &str) -> Vec<Hit> {
    edges_from(graph, caller, RelType::Calls)
}

/// `Calls` edges from `caller` that instantiate `ty`: into a type node named
/// `ty`, or into a constructor of `ty`. Java-family constructors carry the
/// type's name; the others (`constructor`, `__init__`, `init`, `initialize`,
/// `__construct`) carry `ty` as their owner.
pub fn instantiation_calls(graph: &ZeroCopyGraph, caller: &str, ty: &str) -> Vec<Hit> {
    calls_from(graph, caller)
        .into_iter()
        .filter(|h| {
            (h.kind.is_type() && h.name == ty)
                || (h.kind == NodeKind::Constructor && (h.owner == ty || h.name == ty))
        })
        .collect()
}

pub fn assert_calls_constructor(graph: &ZeroCopyGraph, caller: &str, ty: &str, file: &str) {
    let hits = instantiation_calls(graph, caller, ty);
    assert!(
        hits.len() == 1 && hits[0].kind == NodeKind::Constructor && hits[0].file == file,
        "`{caller}` must call the one constructor of `{ty}` in {file}, exactly once; \
         instantiation calls: {hits:?}; all calls: {:?}",
        calls_from(graph, caller)
    );
}

/// `kind` is the type node's kind: `Class`, or `Struct` for a value type.
pub fn assert_calls_type(
    graph: &ZeroCopyGraph,
    caller: &str,
    ty: &str,
    kind: NodeKind,
    file: &str,
) {
    let hits = instantiation_calls(graph, caller, ty);
    assert!(
        hits.len() == 1 && hits[0].kind == kind && hits[0].name == ty && hits[0].file == file,
        "`{caller}` must call the {kind:?} node `{ty}` in {file}, exactly once; \
         instantiation calls: {hits:?}; all calls: {:?}",
        calls_from(graph, caller)
    );
}

pub fn assert_no_instantiation_call(graph: &ZeroCopyGraph, caller: &str, ty: &str) {
    let hits = instantiation_calls(graph, caller, ty);
    assert!(
        hits.is_empty(),
        "`{caller}` must not get a constructor call into `{ty}`; got {hits:?}"
    );
}

/// `caller` holds exactly one `Calls` edge, and it lands on the callable
/// `callee` in `file`. The fixtures keep the caller's body to that one call,
/// so a second edge from the same call site fails here.
pub fn assert_only_call(graph: &ZeroCopyGraph, caller: &str, callee: &str, file: &str) {
    let hits = calls_from(graph, caller);
    assert!(
        hits.len() == 1
            && hits[0].name == callee
            && hits[0].kind.is_callable()
            && hits[0].file == file,
        "`{caller}` must hold one Calls edge, to the callable `{callee}` in {file}; got {hits:?}"
    );
}

/// Pins the existing `Accesses` edge from `caller` to the type `ty`.
pub fn assert_accesses_type(graph: &ZeroCopyGraph, caller: &str, ty: &str) {
    let hits = edges_from(graph, caller, RelType::Accesses);
    assert!(
        hits.iter().any(|h| h.kind.is_type() && h.name == ty),
        "`{caller}` must keep its Accesses edge to `{ty}`; Accesses: {hits:?}"
    );
}
