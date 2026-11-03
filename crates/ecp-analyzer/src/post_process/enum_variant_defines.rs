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
use ecp_core::analyzer::containment::{self, Row, ENUM_VARIANT_DEFINES_REASON};
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::graph::{Edge, RelType};
use ecp_core::pool::StringPool;
use ecp_core::registry::os_path_to_slash;
use rayon::prelude::*;

/// Emit `(Enum)-[:Defines]->(EnumVariant)` edges for all files.
///
/// `owner_class` on each `EnumVariant` was set either by the per-language
/// parser (Rust explicit walk; Kotlin `enum_entry.name` capture) or by
/// `stamp_owner_class_by_span` (TS / Java / C# / Swift / Dart / C++). Either
/// way, `owner_class == Some(enum_name)` names the enum, and the span of
/// the enum row of that name that contains the variant picks it. The pairing
/// is [`containment::enum_variant_defines`], which the overlay's per-file
/// replay shares; this pass supplies each row's node through `raw_map`, Pass
/// 1's map over `local_graphs` in the same order, and the declarations of a
/// variant no enum span contains through `SymbolTable::first_declaration`.
///
/// Returns the total number of edges appended to `edges_out`.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add(ENUM_VARIANT_DEFINES_REASON);

    let chunk_results: Vec<Vec<Edge>> = local_graphs
        .par_iter()
        .enumerate()
        .map(|(file_ord, local_graph)| {
            let path_str_cow = os_path_to_slash(&local_graph.file_path);
            let path_str: &str = &path_str_cow;

            let file = FileOrd(file_ord as u32);
            let mut local_edges: Vec<Edge> = Vec::new();
            containment::enum_variant_defines(
                &local_graph.nodes,
                Row::from,
                |raw| raw_map.node(file, RawIdx(raw as u32)),
                |name, kind, owner| symbol_table.first_declaration(path_str, name, kind, owner),
                |source, target| {
                    local_edges.push(Edge {
                        source,
                        target,
                        rel_type: RelType::Defines,
                        confidence: 1.0,
                        reason,
                    })
                },
            );
            local_edges
        })
        .collect();

    let mut total = 0usize;
    for mut local_edges in chunk_results {
        total += local_edges.len();
        edges_out.append(&mut local_edges);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::types::{LocalGraph, RawNode};
    use ecp_core::graph::NodeKind;
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
            field_reads: Vec::new(),
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
            field_reads: Vec::new(),
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
            route_receivers: None,
            framework_refs: Vec::new(),
            fanout_refs: Vec::new(),
            blind_spots: Vec::new(),
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: Vec::new(),
            raw_function_metas: vec![],
            export_aliases: None,
            parsed_language: ecp_core::contracts::routing::ParsedLanguage::ABSENT,
        }
    }

    fn run(local_graphs: Vec<LocalGraph>) -> Vec<Edge> {
        let mut symbol_table = SymbolTable::new();
        let mut current = 0u32;
        for lg in &local_graphs {
            let path_str = os_path_to_slash(&lg.file_path);
            for rn in &lg.nodes {
                match rn.owner_class.as_deref() {
                    Some(owner) => symbol_table
                        .register_node_owned(&path_str, &rn.name, current, rn.kind, owner),
                    None => symbol_table.register_node(&path_str, &rn.name, current, rn.kind),
                }
                current += 1;
            }
        }
        let raw_map = RawIndexMap::new(0, local_graphs.iter().map(|lg| lg.nodes.len())).unwrap();
        let mut string_pool = StringPool::new();
        let mut edges = Vec::new();
        emit_edges(
            &local_graphs,
            &raw_map,
            &symbol_table,
            &mut string_pool,
            &mut edges,
        );
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
    fn test_emit_edges_shared_variant_name_binds_each_enums_own_variant() {
        let nodes = vec![
            raw("Shape", NodeKind::Enum, (1, 0, 3, 0)),
            raw_variant("None", (2, 4, 2, 8), "Shape"),
            raw("Fill", NodeKind::Enum, (5, 0, 7, 0)),
            raw_variant("None", (6, 4, 6, 8), "Fill"),
        ];
        let edges = run(vec![lg("shared.rs", nodes)]);
        let mut pairs: Vec<(u32, u32)> = edges.iter().map(|e| (e.source, e.target)).collect();
        pairs.sort_unstable();
        assert_eq!(pairs, vec![(0, 1), (2, 3)]);
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

    /// Stands for: enum containment binds by span and row (C1-identity.md "Edge
    /// sources bind by row"). `#[cfg]` twins `enum E { X }` each define their
    /// own `X`; the second enum's variant never hangs off the first enum.
    #[test]
    fn test_emit_edges_same_name_enums_each_define_own_variant() {
        let nodes = vec![
            raw("E", NodeKind::Enum, (1, 0, 3, 0)),
            raw_variant("X", (2, 4, 2, 5), "E"),
            raw("E", NodeKind::Enum, (5, 0, 7, 0)),
            raw_variant("X", (6, 4, 6, 5), "E"),
        ];
        let edges = run(vec![lg("twins.rs", nodes)]);
        let mut pairs: Vec<(u32, u32)> = edges.iter().map(|e| (e.source, e.target)).collect();
        pairs.sort_unstable();
        assert_eq!(pairs, vec![(0, 1), (2, 3)]);
    }

    /// Ends come from the raw map of the file's own input: the second file
    /// binds its own slots.
    #[test]
    fn test_emit_edges_second_file_binds_its_own_slots() {
        let edges = run(vec![
            lg("a.rs", vec![raw("A", NodeKind::Enum, (1, 0, 1, 9))]),
            lg(
                "b.rs",
                vec![
                    raw("B", NodeKind::Enum, (1, 0, 3, 0)),
                    raw_variant("V", (2, 4, 2, 5), "B"),
                ],
            ),
        ]);
        let pairs: Vec<(u32, u32)> = edges.iter().map(|e| (e.source, e.target)).collect();
        assert_eq!(pairs, vec![(1, 2)]);
    }
}
