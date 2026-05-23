//! `TransactionScope` Node emission + `OpensTxScope` edge construction (T10).
//!
//! Bridges the gap between per-file `RawTxScope` entries (populated by
//! per-language parsers via decorator / annotation detection) and the queryable
//! graph layer.
//!
//! ## Algorithm
//!
//! For each `LocalGraph` that carries `tx_scopes` (populated by per-language
//! parsers — Spring `@Transactional`, Django `@transaction.atomic`, .NET
//! `[Transactional]`, Symfony `#[Transactional]`, Pony `@db_session`):
//!
//! 1. **Compute global node base** — prefix-sum over `local_graphs[0..lg_idx].nodes.len()`
//!    gives the global node ID offset for each file's raw nodes. `RawTxScope.node_idx()`
//!    is an index INTO the local graph's `nodes` slice; adding the base gives the
//!    globally-unique node ID written into the `Edge.source` field.
//!
//! 2. **Emit `TransactionScope` node** — one synthetic node per `RawTxScope`.
//!    Name: `tx_scope:{enclosing_fn_name}#{framework_as_str}` — deterministic
//!    from the enclosing function name + framework label so the UID is stable
//!    across re-indexes (assuming function names are stable, which they are for
//!    decorator-annotated functions).
//!
//! 3. **Emit `OpensTxScope` edge** — `source = enclosing_fn_global_id`,
//!    `target = new_tx_scope_node_id`. Confidence 1.0 — decorator presence is
//!    definitive, not heuristic.
//!
//! ## Why a synthetic node (not a direct Function → Function edge)
//!
//! Mirrors the `EventTopic` / `SchemaField` precedent (#275, #272): an
//! intermediate node lets LLMs query
//! `MATCH (f:Function)-[:OpensTxScope]->(s:TransactionScope)` to find all
//! transactional boundaries without scanning decorator strings or call-site
//! patterns. The `TransactionScope` node carries `framework_as_str` in its
//! name for direct `WHERE s.name CONTAINS 'spring'` filtering.

use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::{StrRef, StringPool};
use ecp_core::uid;

/// Emit one `NodeKind::TransactionScope` node + one `OpensTxScope` edge per
/// `RawTxScope` across all local graphs. Returns the count of emitted edges.
///
/// Must run BEFORE the File-node append loop in `builder::build()` so that
/// `TransactionScope` node indices stay in the raw-symbols-and-extras region
/// (same placement as `SchemaField` and `EventTopic` nodes — keeps
/// `file_node_idx` registration correct downstream).
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add("post_process:tx_scope");
    let mut emitted = 0usize;

    // Prefix-sum base: the global node ID for local_graphs[0] starts at 0,
    // for local_graphs[1] at local_graphs[0].nodes.len(), and so on.
    // Mirrors the `node_offset` accumulator in the `function_metas` pass
    // (builder.rs:977-1017).
    let mut node_base: u32 = 0;

    for (lg_idx, local_graph) in local_graphs.iter().enumerate() {
        let Some(ref tx_scopes) = local_graph.tx_scopes else {
            node_base += local_graph.nodes.len() as u32;
            continue;
        };
        if tx_scopes.is_empty() {
            node_base += local_graph.nodes.len() as u32;
            continue;
        }

        let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
        // `file_idx` on synthetic nodes mirrors the EventTopic / SchemaField
        // convention: enumeration index into `local_graphs` (= files[] in
        // the built graph). NOT node_base — that is the node ID offset, not
        // the files[] index.
        let file_idx = lg_idx as u32;

        for raw in tx_scopes.iter() {
            let local_idx = raw.node_idx() as usize;
            // Guard: node_idx must reference a valid slot in this local graph.
            let Some(enclosing_node) = local_graph.nodes.get(local_idx) else {
                continue;
            };

            let fn_global_id = node_base + raw.node_idx();
            let framework_str = raw.framework().as_str();

            // Deterministic name: enclosing function name + framework label.
            let scope_name = format!("tx_scope:{}#{}", enclosing_node.name, framework_str);
            let scope_name_ref: StrRef = string_pool.add(&scope_name);
            let scope_uid = uid::compute(NodeKind::TransactionScope, &path_str, None, &scope_name);

            let tx_node_idx = nodes.len() as u32;
            nodes.push(Node {
                uid: scope_uid,
                name: scope_name_ref,
                file_idx,
                kind: NodeKind::TransactionScope,
                span: enclosing_node.span,
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            });

            edges.push(Edge {
                source: fn_global_id,
                target: tx_node_idx,
                rel_type: RelType::OpensTxScope,
                confidence: 1.0,
                reason,
            });
            emitted += 1;
        }

        node_base += local_graph.nodes.len() as u32;
    }

    emitted
}
