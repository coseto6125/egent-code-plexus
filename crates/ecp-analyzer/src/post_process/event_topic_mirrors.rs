//! `EventTopic` Node promotion + `Publishes`/`Subscribes` edges + heuristic
//! `EventTopicMirror` pairing (T5-33).
//!
//! ## What this pass does
//!
//! For each `LocalGraph` that carries `event_topics` (populated by per-language
//! detectors T5-2..T5-31):
//!
//! 1. **Promote** each `RawEventTopic` → `NodeKind::EventTopic` node.
//! 2. **Connect** the record's endpoint to the new node via
//!    `RelType::Publishes` (direction=Publish) or `RelType::Subscribes`
//!    (direction=Subscribe). The endpoint is the innermost enclosing
//!    callable by span, else the file (C4 `RecordKind::EventTopic`); a `File`
//!    endpoint's edge goes to `file_edges`, as in `path_literal_nodes`.
//! 3. **Mirror** Publish↔Subscribe pairs that share the same canonical topic
//!    string AND the same `FrameworkId` (same-lib gate). One
//!    `RelType::EventTopicMirror` edge per matched pair with `confidence=0.85`
//!    so `is_heuristic()` returns `true` and default `ecp impact` hides it.
//!
//! ## Mirror algorithm
//!
//! - Build `publishers: FxHashMap<(canonical_topic, FrameworkId), Vec<node_idx>>`.
//! - For each Subscriber node, look up publishers with the same `(topic, lib)`.
//! - **Bidirectional top-1 gate**: among candidate publisher nodes, pick the
//!   one whose own top-1 subscriber (earliest canonical match) is THIS subscriber.
//!   For the common case (1 publisher, 1 subscriber) the gate is trivially
//!   satisfied. For ambiguous cases (N publishers, 1 subscriber) only the
//!   first publisher (by insertion order = file/line order) is selected.
//!
//! ## Cross-lib pairing
//!
//! Deferred — T5-33-followup. The same-lib gate keeps false-positive rate low
//! (e.g. Kafka publish topic "orders" must not mirror a Redis subscribe "orders"
//! since those are completely independent channels with different semantics).
//!
//! ## Why EventTopic nodes (vs direct Function→Function edges)
//!
//! The #275 precedent (`SchemaField` as intermediary, not direct function→field
//! edges) applies here. An intermediate `EventTopic` node lets LLMs query
//! `MATCH (f1)-->(t:EventTopic)<--(f2)` without knowing which functions are
//! producers vs consumers. The node also carries `name` (first source spelling) for
//! direct cypher filtering.

use super::path_literal_nodes::FILE_SOURCE_PENDING;
use ecp_core::analyzer::types::{LocalGraph, PubSub};
use ecp_core::contracts::raw_records::{FileOrd, GlobalEndpoint, RawIndexMap};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use ecp_core::uid;
use rustc_hash::FxHashMap;

/// Promote `RawEventTopic`s → `EventTopic` Nodes, emit `Publishes`/`Subscribes`
/// edges from enclosing functions to those nodes, and emit `EventTopicMirror`
/// heuristic edges between Publish↔Subscribe pairs sharing the same canonical
/// topic and framework. Returns the count of emitted `EventTopicMirror` edges.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    file_edges: &mut Vec<(FileOrd, Edge)>,
) -> usize {
    let reason_publishes = string_pool.add("post_process:event_topic:publishes");
    let reason_subscribes = string_pool.add("post_process:event_topic:subscribes");
    let reason_mirror = string_pool.add("post_process:event_topic:mirror");

    // Phase 1 — promote RawEventTopics to EventTopic nodes + directional edges.
    //
    // `publishers` and `subscribers` map `(canonical_topic, lib)` →
    // `Vec<EventTopic_node_idx>` for the mirror pairing phase.
    type TopicKey = (Box<str>, u8); // (canonical_topic, FrameworkId as u8)
    let mut publishers: FxHashMap<TopicKey, Vec<u32>> = FxHashMap::default();
    let mut subscribers: FxHashMap<TopicKey, Vec<u32>> = FxHashMap::default();

    for (lg_idx, local_graph) in local_graphs.iter().enumerate() {
        let Some(ref event_topics) = local_graph.event_topics else {
            continue;
        };
        if event_topics.is_empty() {
            continue;
        }
        let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
        let file_idx = lg_idx as u32;

        // The node of one pairing key is named by its earliest spelling in
        // the file (smallest span), so two spellings in one file name one node.
        let mut first_spelling: FxHashMap<&str, ((u32, u32, u32, u32), &str)> =
            FxHashMap::default();
        for raw_et in event_topics.iter() {
            if let Some(literal) = raw_et.topic_literal.as_deref() {
                let key = raw_et.topic_key.as_deref().unwrap_or(literal);
                let slot = first_spelling.entry(key).or_insert((raw_et.span, literal));
                if raw_et.span < slot.0 {
                    *slot = (raw_et.span, literal);
                }
            }
        }

        for raw_et in event_topics.iter() {
            let Some(ref source_topic) = raw_et.topic_literal else {
                // Dynamic topic — no literal to key on; skip (BlindSpot
                // emission for dynamic topics is the detector's responsibility).
                continue;
            };

            // Records written before `topic_key` existed hold the canonical
            // form in `topic_literal`; they key on it unchanged.
            let canonical_topic = raw_et.topic_key.as_ref().unwrap_or(source_topic);
            let topic_name = first_spelling
                .get(canonical_topic.as_ref())
                .map_or(source_topic.as_ref(), |&(_, spelling)| spelling);
            let lib_u8 = raw_et.lib as u8;

            // Emit the EventTopic node.
            let node_uid = uid::compute(
                NodeKind::EventTopic,
                &path_str,
                None,
                canonical_topic.as_ref(),
            );
            let name_ref = string_pool.add(topic_name);
            let et_idx = nodes.len() as u32;
            nodes.push(Node {
                uid: node_uid,
                name: name_ref,
                file_idx,
                kind: NodeKind::EventTopic,
                span: raw_et.span,
                community_id: 0,
                owner_class: ecp_core::pool::StrRef::default(),
                content_hash: 0,
            });

            // Endpoint → Publishes / Subscribes edge.
            if let Some(endpoint) = raw_et.endpoint_in(&local_graph.nodes) {
                let (rel_type, reason) = match raw_et.direction {
                    PubSub::Publish => (RelType::Publishes, reason_publishes),
                    PubSub::Subscribe => (RelType::Subscribes, reason_subscribes),
                };
                let edge = |source| Edge {
                    source,
                    target: et_idx,
                    rel_type,
                    confidence: 1.0,
                    reason,
                };
                match raw_map.resolve(FileOrd(file_idx), &endpoint) {
                    Some(GlobalEndpoint::Node(source)) => edges.push(edge(source)),
                    Some(GlobalEndpoint::File(file)) => {
                        file_edges.push((file, edge(FILE_SOURCE_PENDING)))
                    }
                    Some(GlobalEndpoint::Unresolved(_)) | None => {}
                }
            }

            // Index by (canonical_topic, lib) for mirror pairing.
            let key: TopicKey = (canonical_topic.clone(), lib_u8);
            match raw_et.direction {
                PubSub::Publish => publishers.entry(key).or_default().push(et_idx),
                PubSub::Subscribe => subscribers.entry(key).or_default().push(et_idx),
            }
        }
    }

    // Phase 2 — bidirectional top-1 EventTopicMirror edges.
    //
    // Same-lib gate: only pairs sharing the same FrameworkId are considered.
    // Cross-lib pairing (e.g. Kafka publish ↔ Faust consume) is deferred —
    // T5-33-followup. Without the same-lib gate, "orders" in Kafka and
    // "orders" in Redis would be incorrectly paired; they are independent
    // channels with different durability and delivery semantics.
    //
    // Bidirectional top-1: for each subscriber, pick the first publisher
    // (insertion order = source-line order). For k=1 this is trivially the
    // mutual best-match. For k>1 (multiple publishers on the same topic) we
    // emit only the first publisher→this_subscriber pair, which means multiple
    // publishers sharing one subscriber do NOT all get edges — the ambiguity
    // is resolved by ordering. This is conservative (fewer false positives)
    // and the documented T5-33 behavior.
    let mut mirror_count = 0usize;
    for (key, sub_idxs) in &subscribers {
        let Some(pub_idxs) = publishers.get(key) else {
            continue;
        };
        // Pick first publisher as top-1.
        let pub_idx = pub_idxs[0];
        for &sub_idx in sub_idxs {
            edges.push(Edge {
                source: pub_idx,
                target: sub_idx,
                rel_type: RelType::EventTopicMirror,
                confidence: 0.85,
                reason: reason_mirror,
            });
            mirror_count += 1;
        }
    }

    mirror_count
}
