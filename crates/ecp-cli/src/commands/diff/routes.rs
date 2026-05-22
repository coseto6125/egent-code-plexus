//! `routes` section: compare Route nodes between two graph snapshots.
//!
//! Reuses existing `ecp routes` extraction logic via the same
//! `ArchivedNodeKind::Route` + `Engine::load` API used by `commands/routes.rs`.
//!
//! `consumers` carries cross-language Fetches in-edges (consumer file →
//! this Route) resolved at index time by `resolution::builder`. The diff
//! layer surfaces them so `review --verdicts` can escalate severity when
//! a route change has known external consumers (silent-break vector for
//! polyglot monorepos).

use crate::commands::graph_csr::iter_incoming_edges_filtered;
use crate::engine::Engine;
use ecp_core::graph::{ArchivedNodeKind, ArchivedRelType};
use ecp_core::EcpError;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

/// Cross-language consumer of a Route — resolved from a `Fetches` in-edge.
/// `path` is the consumer file (e.g., `src/api/orders.ts`); `confidence`
/// is the resolver tier's URL-match confidence (0.9 for `fetch-url-match`).
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct RouteConsumer {
    pub path: String,
    pub confidence: f32,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub handler_file: String,
    pub handler_line: u32,
    /// Cross-language consumers (Fetches edges pointing at this Route node).
    /// Empty for routes with no statically-resolvable consumer fetch calls.
    /// Skipped from JSON when empty to keep diff payload compact.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub consumers: Vec<RouteConsumer>,
}

/// Compare route identity + handler position only — `consumers` shifts
/// independently of route contract (consumer churn ≠ route change), so
/// we exclude it from the diff classification key.
fn contract_eq(a: &RouteEntry, b: &RouteEntry) -> bool {
    a.method == b.method
        && a.path == b.path
        && a.handler_file == b.handler_file
        && a.handler_line == b.handler_line
}

#[derive(Debug, Serialize, Default)]
pub struct RoutesDiff {
    pub added: Vec<RouteEntry>,
    pub removed: Vec<RouteEntry>,
    pub modified: Vec<RouteChange>,
}

#[derive(Debug, Serialize)]
pub struct RouteChange {
    pub before: RouteEntry,
    pub after: RouteEntry,
}

/// Extract all Route nodes from a graph.bin file.
pub fn extract(graph_path: &Path) -> Result<Vec<RouteEntry>, EcpError> {
    let engine = Engine::load(graph_path)
        .map_err(|e| EcpError::Output(format!("load graph {}: {e}", graph_path.display())))?;
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;

    let mut entries = Vec::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        if !matches!(&node.kind, ArchivedNodeKind::Route) {
            continue;
        }
        let name = node.name.resolve(&graph.string_pool);
        let (method, path) = name.split_once(' ').unwrap_or(("", name));
        let file_node = &graph.files[node.file_idx.to_native() as usize];

        let mut consumers: Vec<RouteConsumer> =
            iter_incoming_edges_filtered(graph, idx as u32, |r| {
                matches!(r, ArchivedRelType::Fetches)
            })
            .filter_map(|(src_idx, edge_idx)| {
                let src_node = graph.nodes.get(src_idx as usize)?;
                let src_file = graph.files.get(src_node.file_idx.to_native() as usize)?;
                let consumer_path = src_file.path.resolve(&graph.string_pool).to_string();
                let confidence = graph.edges[edge_idx as usize].confidence.to_native();
                Some(RouteConsumer {
                    path: consumer_path,
                    confidence,
                })
            })
            .collect();
        // Dedup by path: a file may issue multiple fetch calls at the same
        // URL pattern, but for verdict purposes we surface "this file is
        // affected" once. Keep highest confidence per file.
        consumers.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(b.confidence.total_cmp(&a.confidence))
        });
        consumers.dedup_by(|a, b| a.path == b.path);

        entries.push(RouteEntry {
            method: method.to_string(),
            path: path.to_string(),
            handler_file: file_node.path.resolve(&graph.string_pool).to_string(),
            handler_line: node.span.0.to_native(),
            consumers,
        });
    }
    Ok(entries)
}

pub fn diff(baseline: &[RouteEntry], current: &[RouteEntry]) -> RoutesDiff {
    let key = |r: &RouteEntry| (r.method.clone(), r.path.clone());
    let baseline_map: HashMap<_, _> = baseline.iter().map(|r| (key(r), r)).collect();
    let current_map: HashMap<_, _> = current.iter().map(|r| (key(r), r)).collect();

    let mut out = RoutesDiff::default();
    for (k, b) in &baseline_map {
        match current_map.get(k) {
            None => out.removed.push((*b).clone()),
            Some(c) if !contract_eq(b, c) => out.modified.push(RouteChange {
                before: (*b).clone(),
                after: (*c).clone(),
            }),
            _ => {}
        }
    }
    for (k, c) in &current_map {
        if !baseline_map.contains_key(k) {
            out.added.push((*c).clone());
        }
    }
    // Sort for deterministic output (HashMap iteration is non-deterministic).
    out.added
        .sort_by(|a, b| (&a.method, &a.path).cmp(&(&b.method, &b.path)));
    out.removed
        .sort_by(|a, b| (&a.method, &a.path).cmp(&(&b.method, &b.path)));
    out.modified
        .sort_by(|a, b| (&a.after.method, &a.after.path).cmp(&(&b.after.method, &b.after.path)));
    out
}
