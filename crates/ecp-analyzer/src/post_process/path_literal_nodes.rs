//! `PathLiteral` Node emission + `UsesPathLiteral` edge from the literal's
//! endpoint, captured by per-language `path_literals` extractors.
//!
//! Promotes each `LocalGraph.path_literals` entry to:
//!   - One `NodeKind::PathLiteral` Node with `name = literal value`,
//!     `span = literal span`, `owner_class` = the bound node's owner.
//!   - One `UsesPathLiteral` Edge (endpoint → PathLiteral idx) with
//!     `reason = pre-rendered sink_reason` from the extractor.
//!
//! The endpoint is the innermost enclosing callable or `Const` by span, else
//! the file (C4 `RecordKind::PathLiteral`). A `File` endpoint goes to
//! `file_edges`: the builder appends `File` nodes after this pass and gives
//! those edges their source then (C4 Open point 3, option b).

use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::{FileOrd, GlobalEndpoint, RawEndpoint, RawIndexMap};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use ecp_core::uid;

/// The source of an edge in `file_edges` until the builder sets it to the
/// file's `File` node.
pub const FILE_SOURCE_PENDING: u32 = u32::MAX;

/// Returns the number of `UsesPathLiteral` edges appended to `edges` and
/// `file_edges`. PathLiteral node count is derivable from `nodes.len()` delta
/// and is logged separately by the builder.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    file_edges: &mut Vec<(FileOrd, Edge)>,
) -> usize {
    let mut edge_count = 0usize;

    for (lg_idx, lg) in local_graphs.iter().enumerate() {
        let Some(ref literals) = lg.path_literals else {
            continue;
        };
        if literals.is_empty() {
            continue;
        }
        let path_str = lg.file_path.to_string_lossy().replace('\\', "/");
        let file_idx = lg_idx as u32;
        let file = FileOrd(file_idx);

        for raw in literals.iter() {
            let value = raw.value.as_str();
            let endpoint = raw.endpoint_in(&lg.nodes);
            // The owner is the bound node's own owner, so two identical
            // literals in same-name methods of two types get distinct UIDs.
            // A literal with no bound node (a class-body field initializer
            // binds File) keeps the class its walker found, or two classes
            // of one file holding the same literal would share a uid.
            let owner_name = match &endpoint {
                Some(RawEndpoint::Node(idx)) => lg
                    .nodes
                    .get(idx.index())
                    .and_then(|n| n.owner_class.as_deref()),
                _ => raw.enclosing_owner.as_deref(),
            }
            .unwrap_or("");

            let node_uid = uid::compute(
                NodeKind::PathLiteral,
                &path_str,
                if owner_name.is_empty() {
                    None
                } else {
                    Some(owner_name)
                },
                value,
            );
            let name_ref = string_pool.add(value);
            let owner_ref = string_pool.add(owner_name);

            let lit_idx = nodes.len() as u32;
            nodes.push(Node {
                uid: node_uid,
                name: name_ref,
                file_idx,
                kind: NodeKind::PathLiteral,
                span: raw.span,
                community_id: 0,
                owner_class: owner_ref,
                content_hash: 0,
            });

            let Some(endpoint) = endpoint else {
                continue;
            };
            let reason = string_pool.add(&raw.sink_reason);
            let edge = |source| Edge {
                source,
                target: lit_idx,
                rel_type: RelType::UsesPathLiteral,
                confidence: 1.0,
                reason,
            };
            match raw_map.resolve(file, &endpoint) {
                Some(GlobalEndpoint::Node(source)) => edges.push(edge(source)),
                Some(GlobalEndpoint::File(file)) => {
                    file_edges.push((file, edge(FILE_SOURCE_PENDING)))
                }
                Some(GlobalEndpoint::Unresolved(_)) | None => continue,
            }
            edge_count += 1;
        }
    }

    edge_count
}
