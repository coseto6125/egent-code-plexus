//! `contracts` section: compare cross-repo contracts (Route producers and
//! Fetches consumers) between two graph snapshots.
//!
//! # Extraction approach (simplification rationale)
//!
//! `commands/contracts.rs` is a multi-repo stub (extraction body returns empty
//! vecs pending future porting). For the diff layer we extract per-graph:
//!
//! - `kind = "route"` — every `NodeKind::Route` node (producer side).
//!   `identifier` = `"<METHOD> <path>"` (same format as routes.rs).
//!   `schema_keys` = `response_keys` from the matching `RouteShape`, if any.
//!
//! - `kind = "fetch"` — every `RelType::Fetches` edge (consumer side).
//!   `identifier` = caller-node name (the `reason` field of the edge stores
//!   the target URL/path pattern when available; we fall back to the source
//!   node name so the identifier is always non-empty).
//!   `schema_keys` = empty (consumer shape extraction is deferred).
//!
//! This gives a meaningful diff surface without replicating the unfinished
//! multi-repo producer↔consumer matching pipeline.

use crate::engine::Engine;
use cgn_core::graph::{ArchivedNodeKind, ArchivedRelType};
use cgn_core::CgnError;
use serde::Serialize;
use rustc_hash::FxHashMap;
use std::path::Path;

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ContractEntry {
    pub kind: String,
    pub identifier: String,
    pub schema_keys: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct ContractsDiff {
    pub added: Vec<ContractEntry>,
    pub removed: Vec<ContractEntry>,
    pub modified: Vec<ContractChange>,
}

#[derive(Debug, Serialize)]
pub struct ContractChange {
    pub before: ContractEntry,
    pub after: ContractEntry,
}

/// Extract contract entries from a graph.bin file.
///
/// Yields one entry per Route node (kind="route") and one per Fetches edge
/// (kind="fetch"). See module doc for the simplification rationale.
pub fn extract(graph_path: &Path) -> Result<Vec<ContractEntry>, CgnError> {
    let engine = Engine::load(graph_path)
        .map_err(|e| CgnError::Output(format!("load graph {}: {e}", graph_path.display())))?;
    let graph = engine.graph().map_err(|e| CgnError::Rkyv(e.to_string()))?;

    // Build route_shape lookup: node_idx → response_keys.
    let shape_lookup: FxHashMap<u32, Vec<String>> = graph
        .route_shapes
        .iter()
        .map(|rs| {
            let keys = rs
                .response_keys
                .iter()
                .map(|k| k.resolve(&graph.string_pool).to_string())
                .collect();
            (rs.node_idx.to_native(), keys)
        })
        .collect();

    let mut entries = Vec::new();

    // Route nodes → "route" contracts.
    for (idx, node) in graph.nodes.iter().enumerate() {
        if !matches!(&node.kind, ArchivedNodeKind::Route) {
            continue;
        }
        let name = node.name.resolve(&graph.string_pool);
        let schema_keys = shape_lookup.get(&(idx as u32)).cloned().unwrap_or_default();
        entries.push(ContractEntry {
            kind: "route".into(),
            identifier: name.to_string(),
            schema_keys,
        });
    }

    // Fetches edges → "fetch" contracts (consumer side).
    for edge in graph.edges.iter() {
        if !matches!(&edge.rel_type, ArchivedRelType::Fetches) {
            continue;
        }
        let reason = edge.reason.resolve(&graph.string_pool);
        // Use the reason (target URL pattern) as identifier when non-empty;
        // otherwise fall back to the source node name.
        let identifier = if reason.is_empty() {
            let src_idx = edge.source.to_native() as usize;
            graph
                .nodes
                .get(src_idx)
                .map(|n| n.name.resolve(&graph.string_pool).to_string())
                .unwrap_or_default()
        } else {
            reason.to_string()
        };
        entries.push(ContractEntry {
            kind: "fetch".into(),
            identifier,
            schema_keys: Vec::new(),
        });
    }

    Ok(entries)
}

pub fn diff(baseline: &[ContractEntry], current: &[ContractEntry]) -> ContractsDiff {
    let key = |c: &ContractEntry| (c.kind.clone(), c.identifier.clone());
    let baseline_map: FxHashMap<_, _> = baseline.iter().map(|c| (key(c), c)).collect();
    let current_map: FxHashMap<_, _> = current.iter().map(|c| (key(c), c)).collect();

    let mut out = ContractsDiff::default();
    for (k, b) in &baseline_map {
        match current_map.get(k) {
            None => out.removed.push((*b).clone()),
            Some(c) if *c != *b => out.modified.push(ContractChange {
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
        .sort_by(|a, b| (&a.kind, &a.identifier).cmp(&(&b.kind, &b.identifier)));
    out.removed
        .sort_by(|a, b| (&a.kind, &a.identifier).cmp(&(&b.kind, &b.identifier)));
    out.modified.sort_by(|a, b| {
        (&a.after.kind, &a.after.identifier).cmp(&(&b.after.kind, &b.after.identifier))
    });
    out
}
