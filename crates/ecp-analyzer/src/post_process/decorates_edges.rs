//! `Decorates` edge emission — walks every node's `RawNode.decorators` and
//! emits each edge the shared derivation
//! ([`ecp_core::analyzer::decorates::decorates_edges`]) yields: from the
//! decorated symbol to the declaration the `Resolver` resolves the
//! decorator name to as a type, or to a synthetic `Annotation` node.
//!
//! Deduplication of synthetic Annotation nodes: one node per unique
//! `full_name` string across ALL local graphs (keyed by the dotted /
//! fully-qualified form, not the lookup name), uid by
//! [`ecp_core::analyzer::decorates::annotation_uid`]. The overlay replays a
//! dirty file's edges through the same derivation (`session::view`).
//!
//! `file_idx` for synthetic nodes is set to `u32::MAX` (sentinel for "no
//! single owning file") — consistent with `EventTopic` synthetic nodes.
//!
//! Languages wired: Python / TypeScript / JavaScript / Java / Kotlin /
//! C# / Rust / PHP / Swift / Go / C / C++. Ruby and Dart record no
//! decorators.

use crate::resolution::index::ResolveTarget;
use crate::resolution::resolver::Resolver;
use ecp_core::analyzer::decorates::{annotation_uid, decorates_edges, DECORATES_REASON};
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::{FxHashMap, FxHashSet};

/// Emit `Decorates` edges for all local graphs.
///
/// `nodes` is extended with synthetic `Annotation` nodes for unresolved
/// decorators (deduped by full name). Returns the number of `Decorates`
/// edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    resolver: &Resolver<'_>,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add(DECORATES_REASON);
    // Decorator full_name → synthetic node index.
    let mut synthetic: FxHashMap<String, u32> = FxHashMap::default();
    // One (source, target) set for the whole build: a tombstoned twin's
    // slot merges into its survivor's.
    let mut seen: FxHashSet<(u32, u32)> = FxHashSet::default();
    let mut emitted = 0usize;

    for (file_idx, local_graph) in local_graphs.iter().enumerate() {
        let file = FileOrd(file_idx as u32);
        for (raw_idx, raw_node) in local_graph.nodes.iter().enumerate() {
            if raw_node.decorators.is_empty() {
                continue;
            }
            let Some(source) = raw_map.node(file, RawIdx(raw_idx as u32)) else {
                continue;
            };
            decorates_edges(
                source,
                raw_node.kind,
                &raw_node.decorators,
                &mut seen,
                |lookup_name| {
                    resolver.resolve_symbol(
                        &local_graph.file_path,
                        lookup_name,
                        &local_graph.imports,
                        ResolveTarget::Type,
                    )
                },
                |full_name| {
                    if let Some(&idx) = synthetic.get(full_name) {
                        return idx;
                    }
                    let idx = nodes.len() as u32;
                    nodes.push(Node {
                        uid: annotation_uid(full_name),
                        name: string_pool.add(full_name),
                        // `Decorates` edges from many files target this one
                        // node, so no single owning file applies. Consumers
                        // that index `graph.files[file_idx]` must guard via
                        // `Node::has_owning_file()`.
                        file_idx: ecp_core::graph::SYNTHETIC_FILE_IDX,
                        kind: NodeKind::Annotation,
                        span: (0, 0, 0, 0),
                        community_id: 0,
                        owner_class: ecp_core::pool::StrRef::default(),
                        content_hash: 0,
                    });
                    synthetic.insert(full_name.to_owned(), idx);
                    idx
                },
                |target, confidence| {
                    edges.push(Edge {
                        source,
                        target,
                        rel_type: RelType::Decorates,
                        confidence,
                        reason,
                    });
                    emitted += 1;
                },
            );
        }
    }

    emitted
}
