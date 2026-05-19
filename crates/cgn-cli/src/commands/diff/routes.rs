//! `routes` section: compare Route nodes between two graph snapshots.
//!
//! Reuses existing `cgn routes` extraction logic via the same
//! `ArchivedNodeKind::Route` + `Engine::load` API used by `commands/routes.rs`.

use crate::engine::Engine;
use cgn_core::graph::ArchivedNodeKind;
use cgn_core::GnxError;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub handler_file: String,
    pub handler_line: u32,
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
pub fn extract(graph_path: &Path) -> Result<Vec<RouteEntry>, GnxError> {
    let engine = Engine::load(graph_path)
        .map_err(|e| GnxError::Output(format!("load graph {}: {e}", graph_path.display())))?;
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    let mut entries = Vec::new();
    for node in graph.nodes.iter() {
        if !matches!(&node.kind, ArchivedNodeKind::Route) {
            continue;
        }
        let name = node.name.resolve(&graph.string_pool);
        let (method, path) = name.split_once(' ').unwrap_or(("", name));
        let file_node = &graph.files[node.file_idx.to_native() as usize];
        entries.push(RouteEntry {
            method: method.to_string(),
            path: path.to_string(),
            handler_file: file_node.path.resolve(&graph.string_pool).to_string(),
            handler_line: node.span.0.to_native(),
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
            Some(c) if *c != *b => out.modified.push(RouteChange {
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
