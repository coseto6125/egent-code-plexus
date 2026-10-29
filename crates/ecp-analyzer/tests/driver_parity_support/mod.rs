//! Helpers shared by the `*_driver_parity.rs` tests: the row shapes and
//! projections that every language file read from a `LocalGraph`, and the
//! empty-graph assertion for the zero-byte case.
#![allow(dead_code)]

use ecp_core::analyzer::types::{FrameworkId, LocalGraph};
use ecp_core::graph::NodeKind;
use std::path::Path;

pub type Span4 = (u32, u32, u32, u32);
pub type Ref<'g> = (&'g str, &'g str, &'g str, f32, Span4);

pub fn names_kinds(g: &LocalGraph) -> Vec<(&str, NodeKind)> {
    g.nodes.iter().map(|n| (n.name.as_str(), n.kind)).collect()
}

pub fn types(g: &LocalGraph) -> Vec<Option<&str>> {
    g.nodes
        .iter()
        .map(|n| n.type_annotation.as_deref())
        .collect()
}

pub fn refs(g: &LocalGraph) -> Vec<Ref<'_>> {
    g.framework_refs
        .iter()
        .map(|r| {
            (
                r.source_name.as_str(),
                r.target_name.as_str(),
                r.reason.as_str(),
                r.confidence,
                r.span,
            )
        })
        .collect()
}

/// `(callable name, framework)` of each tx scope, in emitted order.
pub fn tx(g: &LocalGraph) -> Vec<(&str, FrameworkId)> {
    g.tx_scopes
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| (g.nodes[s.node_idx() as usize].name.as_str(), s.framework()))
        .collect()
}

/// A zero-byte file parses to a graph with no payload in any collection.
pub fn assert_empty_graph(g: &LocalGraph, path: &str) {
    assert_eq!(g.file_path, Path::new(path));
    assert!(g.nodes.is_empty() && g.documents.is_empty() && g.imports.is_empty());
    assert!(g.blind_spots.is_empty() && g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}
