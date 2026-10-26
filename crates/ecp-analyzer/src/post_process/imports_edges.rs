//! `Imports` edge emission — `File` node → imported target.
//!
//! Walks each `LocalGraph.imports` (already populated by the 14 language
//! parsers), resolves each `RawImport` via the same `Resolver` used for
//! Calls/Accesses, and emits one `Imports` edge per
//! `(file_node, resolved_target)` pair.
//!
//! Spec: `docs/superpowers/specs/2026-05-17-imports-edge-emission.md` §2 —
//! resolver miss → don't emit (refuse to produce gitnexus-style cross-language
//! false positives like `.mjs → Path.java`).
//!
//! The steps, their confidences and reasons are
//! [`ecp_core::analyzer::import_edges`], shared with the overlay's replay of a
//! dirty file. This pass answers its lookups from the [`Resolver`]: step 1 by
//! `Resolver::resolve_import` / `Resolver::resolve_import_binding` (confidence
//! by tier: `SameFile=1.0`, `ImportScoped=0.95`, `QualifierScoped=0.85`,
//! `HeritageScoped=0.85`, `Global=0.7`), steps 3a and 3b by
//! `Resolver::enumerate_candidates`, and steps 3c to 3f from one
//! [`GuessIndex`] over `file_node_idx`.

use crate::resolution::index::ResolveTarget;
use crate::resolution::resolver::Resolver;
use ecp_core::analyzer::import_edges::{import_edges, GuessIndex, ImportEdgeFacts, ImportReason};
use ecp_core::analyzer::types::{LocalGraph, RawImport};
use ecp_core::graph::{Edge, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::FxHashMap;
use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

/// Emit `Imports` edges from each `File` node to the resolved targets of
/// every `RawImport` in that file. Returns the number of edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    resolver: &Resolver<'_>,
    file_node_idx: &FxHashMap<String, u32>,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let reason_named = string_pool.add(ImportReason::Named.as_str());
    let reason_module = string_pool.add(ImportReason::Module.as_str());
    let reason_guess = string_pool.add(ImportReason::Guess.as_str());

    // One basename / directory index, built once, so steps 3c to 3f hash
    // into a bucket (typically < 10 paths) instead of scanning every file
    // per import: on `.sample_repo` (14k files) O(imports × files) ≈ 200M
    // comparisons became O(imports × bucket).
    let guesses = GuessIndex::new(
        file_node_idx
            .iter()
            .map(|(path, &idx)| (path.as_str(), idx)),
    );

    // Parallel per file: each file's edges are deduplicated by target, and a
    // file's source node is unique, so per-file dedupe equals a global one.
    use rayon::prelude::*;
    let chunk_results: Vec<Vec<Edge>> = local_graphs
        .par_iter()
        .map(|local_graph| {
            let mut local_edges: Vec<Edge> = Vec::new();
            // Forward-slash normalize without allocating on Linux/macOS
            // (raw path is already `/`-only there). file_node_idx.get() takes
            // &str so Cow → &str → HashMap lookup is alloc-free for the
            // common branch.
            let raw_path = local_graph.file_path.to_string_lossy();
            let path_str: Cow<'_, str> = if raw_path.contains('\\') {
                Cow::Owned(raw_path.replace('\\', "/"))
            } else {
                raw_path
            };
            let Some(&source) = file_node_idx.get(path_str.as_ref()) else {
                return local_edges;
            };
            if local_graph.imports.is_empty() {
                return local_edges;
            }
            let facts = IndexImports {
                resolver,
                file: &local_graph.file_path,
                imports: &local_graph.imports,
                file_node_idx,
                guesses: &guesses,
            };
            import_edges(
                &local_graph.file_path,
                source,
                resolver.file_language(&path_str),
                &local_graph.imports,
                &facts,
                |edge| {
                    local_edges.push(Edge {
                        source,
                        target: edge.target,
                        rel_type: RelType::Imports,
                        confidence: edge.confidence,
                        reason: match edge.reason {
                            ImportReason::Named => reason_named,
                            ImportReason::Module => reason_module,
                            ImportReason::Guess => reason_guess,
                        },
                    })
                },
            );
            local_edges
        })
        .collect();

    // Serial merge, in file order.
    let emitted: usize = chunk_results.iter().map(Vec::len).sum();
    edges_out.reserve(emitted);
    for edges in chunk_results {
        edges_out.extend(edges);
    }
    emitted
}

/// The index's answers to [`ImportEdgeFacts`] for one importer.
struct IndexImports<'r, 'a, 'g> {
    resolver: &'r Resolver<'a>,
    file: &'r Path,
    imports: &'r [RawImport],
    file_node_idx: &'r FxHashMap<String, u32>,
    guesses: &'r GuessIndex<'g>,
}

impl ImportEdgeFacts for IndexImports<'_, '_, '_> {
    fn named(
        &self,
        name: &str,
        target: ResolveTarget,
        binding_only: bool,
    ) -> (Vec<(u32, f32)>, Option<Arc<str>>) {
        match binding_only {
            true => self
                .resolver
                .resolve_import_binding(self.file, name, self.imports, target),
            false => self
                .resolver
                .resolve_import(self.file, name, self.imports, target),
        }
    }

    fn file_node(&self, path: &str) -> Option<u32> {
        self.file_node_idx.get(path).copied()
    }

    fn for_each_candidate(&self, specifier: &str, visit: &mut dyn FnMut(&str) -> bool) {
        self.resolver
            .enumerate_candidates(self.file, specifier, visit);
    }

    fn guesses(&self) -> &GuessIndex<'_> {
        self.guesses
    }
}
