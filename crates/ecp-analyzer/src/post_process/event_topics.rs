//! `EventTopic` Node emission + `Publishes` / `Subscribes` edge construction
//! (T10-4 partial / T5-33 subset).
//!
//! Bridges the gap between per-file `RawEventTopic` (T5-2..T5-31 detectors)
//! and the queryable graph layer: each unique `(enclosing_fn, lib)` pair from
//! a publish call site emits one `NodeKind::EventTopic` Node, and the enclosing
//! function node receives a `RelType::Publishes` outgoing edge to it.
//! Consumer call sites emit `RelType::Subscribes` edges analogously.
//!
//! ## Algorithm
//!
//! 1. **Dedup topics** — group `RawEventTopic` entries by `(enclosing_fn, lib)`
//!    to avoid duplicate `EventTopic` nodes when the same function calls the
//!    same produce API twice. `topic_literal` is used as the EventTopic name
//!    when available (dynamic topics with `None` are skipped — no fabricated
//!    nodes).
//!
//! 2. **Emit edges** — for each unique publish/subscribe site, resolve
//!    `enclosing_fn` via `SymbolTable` to find the owning Function/Method
//!    node in the same file. On miss, silently drop (no dangling edge).
//!
//! ## Scope note
//!
//! This module intentionally does NOT emit `EventTopicMirror` heuristic
//! edges — that is T5-33's scope (requires at least one Publish + one
//! Subscribe detector per lib to be gated correctly per D7). The mirror
//! edges will be added by T5-33 without touching this file.

use crate::resolution::index::SymbolTable;
use ecp_core::analyzer::types::{LocalGraph, PubSub};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::{StrRef, StringPool};
use ecp_core::uid;
use rustc_hash::FxHashMap;

/// Promote `RawEventTopic`s to `EventTopic` Nodes + `Publishes` / `Subscribes`
/// edges.  Returns the count of emitted `Publishes` + `Subscribes` edges.
///
/// Must run BEFORE the File-node append loop in `builder::build()` so that
/// `EventTopic` node indices stay in the raw-symbols-and-extras region
/// (consistent with how `SchemaField` nodes are placed by `schema_field_mirrors`).
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> usize {
    let reason_pub = string_pool.add("post_process:event_topic:publishes");
    let reason_sub = string_pool.add("post_process:event_topic:subscribes");

    // Dedup map: normalised topic name → EventTopic node idx.
    let mut topic_node_idx: FxHashMap<String, u32> = FxHashMap::default();

    let mut emitted = 0usize;

    for (lg_idx, local_graph) in local_graphs.iter().enumerate() {
        let Some(ref event_topics) = local_graph.event_topics else {
            continue;
        };
        if event_topics.is_empty() {
            continue;
        }

        let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
        let file_idx = lg_idx as u32;

        for raw_et in event_topics.iter() {
            // Skip dynamic topics without a literal.
            let Some(ref topic_lit) = raw_et.topic_literal else {
                continue;
            };

            // Skip empty enclosing function (shouldn't happen in practice).
            if raw_et.enclosing_fn.is_empty() {
                continue;
            }

            // Resolve enclosing function in this file's symbol table.
            let Some(fn_node_idx) = symbol_table.lookup_in_file(&path_str, &raw_et.enclosing_fn)
            else {
                continue;
            };

            // Use the topic literal as the EventTopic name.
            let topic_name: &str = topic_lit;

            // Emit or reuse EventTopic node, keyed by (topic_name, lib) to
            // keep a single canonical node per topic-per-framework.
            let et_key = format!("{}:{}", topic_name, raw_et.lib.as_str());
            let et_node_idx = *topic_node_idx.entry(et_key).or_insert_with(|| {
                let topic_name_ref: StrRef = string_pool.add(topic_name);
                let node_uid = uid::compute(NodeKind::EventTopic, &path_str, None, topic_name);
                let idx = nodes.len() as u32;
                nodes.push(Node {
                    uid: node_uid,
                    name: topic_name_ref,
                    file_idx,
                    kind: NodeKind::EventTopic,
                    span: raw_et.span,
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                });
                idx
            });

            let (rel_type, reason) = match raw_et.direction {
                PubSub::Publish => (RelType::Publishes, reason_pub),
                PubSub::Subscribe => (RelType::Subscribes, reason_sub),
            };

            edges.push(Edge {
                source: fn_node_idx,
                target: et_node_idx,
                rel_type,
                confidence: 0.9,
                reason,
            });
            emitted += 1;
        }
    }

    emitted
}
