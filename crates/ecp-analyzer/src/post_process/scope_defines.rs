//! `Defines` edge emission — scope-containment fill for File / Namespace / Module.
//!
//! Covers scope relationships NOT already handled by `HasMethod` / `HasProperty`:
//!
//! - `File → top-level symbol` (any non-member kind whose `owner_class.is_none()`)
//! - `Namespace → child` (`owner_class == Some(namespace_name)`)
//! - `Module → child` (`owner_class == Some(module_name)`)
//!
//! Class → Method / Property containment stays exclusively HasMethod / HasProperty
//! — Defines must NOT duplicate. The invariant: `owner_class.is_none()` OR the
//! owner is a Namespace/Module (not a Class/Interface/Trait).
//!
//! Which row defines which is [`containment::scope_defines`], the derivation
//! the overlay's per-file replay shares. Both ends come from raw indexes
//! through the build's `RawIndexMap`, never from a name lookup: two same-name
//! declarations each get their own edge, and a merged row (Python `x = 1;
//! x = 2`) maps to its survivor, whose edge it gives once, also when the
//! survivor is a row of an earlier input of the path.
//!
//! A scoped node (a Rust nested `fn`, a closure node) has no owner, yet no
//! File or module declares it: its callable does, through the lexical-parent
//! table. It gets no `Defines` edge.

use ecp_core::analyzer::containment::{self, Row, Scope, SCOPE_DEFINES_REASON};
use ecp_core::analyzer::lexical::SharedPaths;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::graph::{Edge, RelType};
use ecp_core::pool::StringPool;
use ecp_core::registry::os_path_to_slash;
use rustc_hash::{FxHashMap, FxHashSet};

/// Emit `Defines` edges from File / Namespace / Module nodes to their
/// contained symbols. `is_scoped` tells a scoped node by its node id; it
/// gets no edge. `shared` holds the runs of inputs of one path, which share
/// one dedupe set. Returns the number of edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    file_node_idx: &FxHashMap<String, u32>,
    raw_map: &RawIndexMap,
    shared: &SharedPaths,
    is_scoped: impl Fn(u32) -> bool,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add(SCOPE_DEFINES_REASON);
    let mut emitted = 0usize;
    let mut seen: Option<FxHashSet<(u32, u32)>> = None;

    for (lg_idx, local_graph) in local_graphs.iter().enumerate() {
        let path = os_path_to_slash(&local_graph.file_path);
        let file = FileOrd(lg_idx as u32);
        let ord = lg_idx as u32;
        let inputs = shared.run_of(ord).unwrap_or(ord..ord + 1);
        if inputs.start == ord {
            seen = inputs
                .map(FileOrd)
                .any(|input| raw_map.has_merged(input))
                .then(FxHashSet::default);
        }

        let Some(&file_node_id) = file_node_idx.get(path.as_ref()) else {
            continue;
        };
        let node = |raw: usize| raw_map.node(file, RawIdx(raw as u32));

        containment::scope_defines(&local_graph.nodes, Row::from, |scope, child| {
            let Some(child_id) = node(child).filter(|&id| !is_scoped(id)) else {
                return;
            };
            let source = match scope {
                Scope::File => Some(file_node_id),
                Scope::Container(container) => node(container),
            };
            let Some(source) = source else {
                return;
            };
            if seen
                .as_mut()
                .is_none_or(|seen| seen.insert((source, child_id)))
            {
                edges_out.push(Edge {
                    source,
                    target: child_id,
                    rel_type: RelType::Defines,
                    confidence: 1.0,
                    reason,
                });
                emitted += 1;
            }
        });
    }

    emitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::types::RawNode;
    use ecp_core::graph::NodeKind;
    use std::path::PathBuf;

    fn variable(name: &str, line: u32) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind: NodeKind::Variable,
            span: (line, 0, line, 5),
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

    fn defines(local_graphs: &[LocalGraph], merged: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
        let raw_map = RawIndexMap::new(0, local_graphs.iter().map(|lg| lg.nodes.len()))
            .unwrap()
            .with_merged(merged)
            .unwrap();
        let file_nodes: FxHashMap<String, u32> = local_graphs
            .iter()
            .map(|lg| (lg.file_path.to_string_lossy().into_owned(), 100))
            .collect();
        let mut edges = Vec::new();
        emit_edges(
            local_graphs,
            &file_nodes,
            &raw_map,
            &SharedPaths::of(local_graphs),
            |_| false,
            &mut StringPool::new(),
            &mut edges,
        );
        edges.iter().map(|e| (e.source, e.target)).collect()
    }

    /// Stands for: a merged repeat gives one `Defines` (design_uid §2.3),
    /// also when its survivor is a row of an earlier input of its path. Two
    /// inputs of `m.py`, each `x = …`: the second merges into the first.
    #[test]
    fn test_emit_edges_two_inputs_one_path_merged_row_defines_once() {
        let graphs = [
            lg("m.py", vec![variable("x", 0)]),
            lg("m.py", vec![variable("x", 1)]),
        ];
        assert_eq!(defines(&graphs, vec![(1, 0)]), [(100, 0)]);
    }

    /// A merged row and its survivor in one input give one edge; a file of
    /// another path keeps its own.
    #[test]
    fn test_emit_edges_merged_row_one_input_defines_once_other_path_own_edge() {
        let graphs = [
            lg("a.py", vec![variable("x", 0), variable("x", 1)]),
            lg("b.py", vec![variable("x", 0)]),
        ];
        assert_eq!(defines(&graphs, vec![(1, 0)]), [(100, 0), (100, 2)]);
    }

    #[test]
    fn test_emit_edges_no_inputs_emits_nothing() {
        assert!(defines(&[], Vec::new()).is_empty());
    }
}
