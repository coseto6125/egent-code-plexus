use super::config::EventTopicConfig;
use super::normalize::canonicalize;
use crate::framework_helpers::has_import_from;
use ecp_core::analyzer::types::{PubSub, RawEventTopic, RawImport};
use rustc_hash::FxHashMap;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor, Tree};
// Note: StringPool import removed — RawEventTopic now uses owned Box<str>

/// Walk all captures produced by `query` against `tree`/`source`, dispatch
/// each match to the first `EventTopicConfig` whose import-gate is satisfied,
/// and emit a `RawEventTopic` for every accepted match.
///
/// The caller is responsible for supplying a query whose capture names align
/// with the `topic_capture`, `producer_capture`, and `direction_capture`
/// fields of at least one config.  Captures not referenced by any active
/// config are silently ignored — forward-compatible with queries that carry
/// extra context for T5-2..T5-N frameworks.
///
/// # Import-gate semantics
/// A config fires only when `has_import_from(imports, config.import_gate)`
/// returns `true`.  When no config's gate is satisfied by the file's imports,
/// this function returns an empty `Vec` — no false positives.
///
/// # Schema gap (noted for T5-2+)
/// `RawEventTopic` has no `kind` field to distinguish queue-vs-exchange
/// semantics (as AMQP does).  The `direction_classifier` therefore returns
/// `PubSub` (Publish/Subscribe direction), not a queue-topology kind.
/// A `kind` field should be added to `RawEventTopic` in a future
/// append-only schema migration when T5-3 (RabbitMQ) lands.
pub fn extract_event_topics(
    tree: &Tree,
    source: &[u8],
    query: &Query,
    configs: &[EventTopicConfig],
    imports: &[RawImport],
) -> Vec<RawEventTopic> {
    // Identify which configs are live for this file once, not per-match.
    let active: Vec<&EventTopicConfig> = configs
        .iter()
        .filter(|c| has_import_from(imports, c.import_gate))
        .collect();

    if active.is_empty() {
        return Vec::new();
    }

    // Pre-build capture-name → [(active_idx, role)] map once per call.
    // Multiple active configs may share the same capture name, so the value
    // is a Vec. Done once here; amortises across all (match × capture) pairs
    // in the file, keeping the per-capture cost O(1) instead of O(K).
    let mut cap_map: FxHashMap<&str, Vec<(usize, CaptureRole)>> =
        FxHashMap::with_capacity_and_hasher(active.len() * 3, Default::default());
    for (idx, cfg) in active.iter().enumerate() {
        cap_map
            .entry(cfg.topic_capture)
            .or_default()
            .push((idx, CaptureRole::Topic));
        if !cfg.producer_capture.is_empty() {
            cap_map
                .entry(cfg.producer_capture)
                .or_default()
                .push((idx, CaptureRole::Producer));
        }
        if !cfg.direction_capture.is_empty() {
            cap_map
                .entry(cfg.direction_capture)
                .or_default()
                .push((idx, CaptureRole::Direction));
        }
    }

    let n_active = active.len();
    let mut out = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);

    // Reusable per-match slot table: [topic, producer, direction] text per active config.
    // Allocated once, cleared before each match.
    let mut slots: Vec<[Option<&str>; 3]> = vec![[None; 3]; n_active];

    // Hoist `capture_names()` out of the match × capture nested loop.
    // tree_sitter's `capture_names()` re-walks the query's name table on
    // each call; hoist to avoid O(matches × captures_per_match) overhead.
    let cap_names = query.capture_names();

    while let Some(m) = matches.next() {
        // Reset slots for this match.
        for s in slots.iter_mut() {
            *s = [None; 3];
        }

        // Single O(M) pass over captures; O(1) lookup per capture.
        for cap in m.captures {
            let cap_name = cap_names[cap.index as usize];
            if let Some(entries) = cap_map.get(cap_name) {
                let node_text = cap.node.utf8_text(source).unwrap_or("");
                for &(idx, role) in entries {
                    slots[idx][role as usize] = Some(node_text);
                }
            }
        }

        // Scan active configs in declaration order; first fully-populated wins.
        for (idx, config) in active.iter().enumerate() {
            let [topic_opt, producer_opt, direction_opt] = slots[idx];
            let Some(raw_topic) = topic_opt else {
                continue;
            };

            // String literal nodes include their delimiters (e.g. `"order.created"`);
            // strip them so topic consumers receive the bare string value.
            let bare_topic = strip_string_delimiters(raw_topic);
            let topic_text = if config.canonicalize {
                canonicalize(bare_topic)
            } else {
                bare_topic.to_string()
            };

            let direction_raw = strip_string_delimiters(direction_opt.unwrap_or(""));
            let direction = (config.direction_classifier)(direction_raw);

            let enclosing_fn: Box<str> = producer_opt.unwrap_or("").into();

            let start = m.captures[0].node.start_position();
            let end = m.captures[0].node.end_position();
            let span = (
                start.row as u32,
                start.column as u32,
                end.row as u32,
                end.column as u32,
            );

            out.push(RawEventTopic {
                topic_literal: Some(topic_text.into_boxed_str()),
                direction,
                lib: config.framework,
                enclosing_fn,
                span,
            });
            break;
        }
    }

    out
}

/// Strip leading/trailing string-literal delimiters from a tree-sitter
/// string capture.
///
/// Tree-sitter's `string` nodes include the surrounding quote characters
/// (e.g. `"order.created"` → `order.created`). Single-quoted, double-quoted,
/// and backtick-quoted forms are all handled. Non-string captures (plain
/// identifiers) are returned unchanged — the function is a no-op when the
/// text doesn't start with a recognised delimiter.
fn strip_string_delimiters(s: &str) -> &str {
    let delimiters: &[char] = &['"', '\'', '`'];
    if s.starts_with(|c: char| delimiters.contains(&c)) {
        s.trim_matches(|c: char| delimiters.contains(&c))
    } else {
        s
    }
}

/// Capture role for the dispatch map — maps a slot index to its semantic
/// meaning within `EventTopicConfig`.
#[derive(Clone, Copy)]
enum CaptureRole {
    Topic = 0,
    Producer = 1,
    Direction = 2,
}

/// Direction classifier for Kafka topics.
///
/// Kafka topics have no queue/exchange topology distinction in our model;
/// returns `PubSub::Publish` unconditionally. T5-2 configs can specialise
/// this when a capture reliably identifies subscriber call sites.
pub fn classify_kafka_direction(_raw: &str) -> PubSub {
    PubSub::Publish
}

/// Direction classifier for AMQP / RabbitMQ call sites.
///
/// Maps producer-side call text to `Publish`; subscriber-side call text to
/// `Subscribe`. Unrecognised capture text defaults to `Publish` so the topic
/// is still indexed rather than silently dropped.
///
/// Note: queue-vs-exchange topology cannot be expressed in `PubSub` — see
/// the schema gap note in `extract_event_topics` for the forward path.
pub fn classify_amqp_direction(raw: &str) -> PubSub {
    match raw {
        "consume" | "subscribe" | "basic_consume" | "basic_get" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}
