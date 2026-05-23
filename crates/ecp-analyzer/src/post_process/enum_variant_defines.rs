//! `Defines` edge emission — Enum → EnumVariant containment.
//!
//! Spans `NodeKind::EnumVariant` nodes to their enclosing `NodeKind::Enum`
//! within each file via `owner_class` (set by `stamp_owner_class_by_span`
//! for span-containment languages, or by each parser directly for Rust/Kotlin
//! where the parse tree makes ownership explicit).
//!
//! # Why a separate post-process rather than reusing class_membership
//!
//! `class_membership` emits `HasMethod` / `HasProperty` for Class → member
//! edges. `Enum → EnumVariant` is a *containment* edge (`Defines`), not a
//! method-dispatch edge. Keeping them separate avoids widening
//! `class_membership`'s invariants and makes each pass easier to read.

use crate::resolution::index::SymbolTable;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::borrow::Cow;

/// Emit `(Enum)-[:Defines]->(EnumVariant)` edges for all files.
///
/// `owner_class` on each `EnumVariant` was set either by the per-language
/// parser (Rust explicit walk; Kotlin `enum_entry.name` capture) or by
/// `stamp_owner_class_by_span` (TS / Java / C# / Swift / Dart / C++). Either
/// way, `owner_class == Some(enum_name)` is the link — no additional span
/// arithmetic needed here.
///
/// Returns the total number of edges appended to `edges_out`.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add("post_process:enum_variant_defines");

    let chunk_results: Vec<(usize, Vec<Edge>)> = local_graphs
        .par_iter()
        .map(|local_graph| {
            let raws = &local_graph.nodes;

            // Fast-exit: skip files with no EnumVariant nodes.
            if !raws.iter().any(|n| n.kind == NodeKind::EnumVariant) {
                return (0, Vec::new());
            }

            let raw_path = local_graph.file_path.to_string_lossy();
            let path_str_cow: Cow<'_, str> = if raw_path.contains('\\') {
                Cow::Owned(raw_path.replace('\\', "/"))
            } else {
                raw_path
            };
            let path_str: &str = &path_str_cow;

            // Index Enum nodes in this file by name for O(1) lookup.
            let enum_by_name: FxHashMap<&str, u32> = raws
                .iter()
                .filter(|n| n.kind == NodeKind::Enum)
                .filter_map(|n| {
                    symbol_table
                        .lookup_in_file(path_str, &n.name)
                        .map(|idx| (n.name.as_str(), idx))
                })
                .collect();

            if enum_by_name.is_empty() {
                return (0, Vec::new());
            }

            let mut local_edges: Vec<Edge> = Vec::new();
            let mut emitted = 0usize;

            for variant in raws.iter().filter(|n| n.kind == NodeKind::EnumVariant) {
                let Some(ref enum_name) = variant.owner_class else {
                    continue;
                };
                let Some(&enum_idx) = enum_by_name.get(enum_name.as_str()) else {
                    continue;
                };
                let Some(variant_idx) = symbol_table.lookup_in_file(path_str, &variant.name) else {
                    continue;
                };
                // Self-loop guard: same-named enum and variant (extremely rare).
                if enum_idx == variant_idx {
                    continue;
                }
                local_edges.push(Edge {
                    source: enum_idx,
                    target: variant_idx,
                    rel_type: RelType::Defines,
                    confidence: 1.0,
                    reason,
                });
                emitted += 1;
            }

            (emitted, local_edges)
        })
        .collect();

    let mut total = 0usize;
    for (count, mut local_edges) in chunk_results {
        total += count;
        edges_out.append(&mut local_edges);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::types::{LocalGraph, RawNode};
    use std::path::PathBuf;

    fn raw(name: &str, kind: NodeKind, span: (u32, u32, u32, u32)) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
            owner_class: None,
            content_hash: 0,
        }
    }

    fn raw_variant(name: &str, span: (u32, u32, u32, u32), owner: &str) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind: NodeKind::EnumVariant,
            span,
            is_exported: false,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
            owner_class: Some(owner.to_string()),
            content_hash: 0,
        }
    }

    fn lg(path: &str, nodes: Vec<RawNode>) -> LocalGraph {
        LocalGraph {
            file_path: PathBuf::from(path),
            content_hash: [0u8; 8],
            nodes,
            documents: Vec::new(),
            imports: Vec::new(),
            routes: Vec::new(),
            framework_refs: Vec::new(),
            fanout_refs: Vec::new(),
            blind_spots: Vec::new(),
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            call_metas: Vec::new(),
            raw_function_metas: vec![],
        }
    }

    fn run(local_graphs: Vec<LocalGraph>) -> Vec<Edge> {
        let mut symbol_table = SymbolTable::new();
        let mut current = 0u32;
        for lg in &local_graphs {
            let path_str = lg.file_path.to_string_lossy().replace('\\', "/");
            for rn in &lg.nodes {
                symbol_table.register_node(&path_str, &rn.name, current, rn.kind);
                current += 1;
            }
        }
        let mut string_pool = StringPool::new();
        let mut edges = Vec::new();
        emit_edges(&local_graphs, &symbol_table, &mut string_pool, &mut edges);
        edges
    }

    #[test]
    fn enum_emits_defines_for_each_variant() {
        let nodes = vec![
            raw("Status", NodeKind::Enum, (1, 0, 5, 0)),
            raw_variant("Active", (2, 4, 2, 10), "Status"),
            raw_variant("Inactive", (3, 4, 3, 12), "Status"),
            raw_variant("Pending", (4, 4, 4, 11), "Status"),
        ];
        let edges = run(vec![lg("status.rs", nodes)]);
        assert_eq!(edges.len(), 3);
        for e in &edges {
            assert!(matches!(e.rel_type, RelType::Defines));
            assert_eq!(e.source, 0); // Status enum idx
        }
        let targets: std::collections::HashSet<u32> = edges.iter().map(|e| e.target).collect();
        assert!(targets.contains(&1)); // Active
        assert!(targets.contains(&2)); // Inactive
        assert!(targets.contains(&3)); // Pending
    }

    #[test]
    fn no_variants_no_edges() {
        let nodes = vec![raw("Color", NodeKind::Enum, (1, 0, 3, 0))];
        let edges = run(vec![lg("color.rs", nodes)]);
        assert_eq!(edges.len(), 0);
    }

    #[test]
    fn variant_without_owner_class_skipped() {
        let nodes = vec![
            raw("Dir", NodeKind::Enum, (1, 0, 4, 0)),
            raw("Left", NodeKind::EnumVariant, (2, 4, 2, 8)),
        ];
        // Left has no owner_class — should produce 0 edges.
        let edges = run(vec![lg("dir.rs", nodes)]);
        assert_eq!(edges.len(), 0);
    }

    #[test]
    fn two_enums_same_file_emit_independently() {
        let nodes = vec![
            raw("Direction", NodeKind::Enum, (1, 0, 5, 0)),
            raw_variant("Left", (2, 4, 2, 8), "Direction"),
            raw_variant("Right", (3, 4, 3, 9), "Direction"),
            raw("Color", NodeKind::Enum, (7, 0, 11, 0)),
            raw_variant("Red", (8, 4, 8, 7), "Color"),
            raw_variant("Blue", (9, 4, 9, 8), "Color"),
        ];
        let edges = run(vec![lg("mixed.rs", nodes)]);
        assert_eq!(edges.len(), 4);
        let dir_idx = 0u32; // Direction
        let col_idx = 3u32; // Color
        assert!(edges.iter().filter(|e| e.source == dir_idx).count() == 2);
        assert!(edges.iter().filter(|e| e.source == col_idx).count() == 2);
    }
}
