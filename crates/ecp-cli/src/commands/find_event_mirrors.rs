//! `ecp find-event-mirrors` — list `EventTopicMirror` heuristic edges (T5-34).
//!
//! ## What this does
//!
//! Scans the indexed graph for all `EventTopicMirror` edges emitted by the
//! T5-33 post-process pass and surfaces each paired (publisher_fn,
//! subscriber_fn, topic, confidence) tuple so LLMs can reason about
//! publish/subscribe event flow without knowing producer/consumer file paths.
//!
//! ## Graph shape walked
//!
//! ```text
//! (publisher_fn) -[Publishes]-> (EventTopic:pub) -[EventTopicMirror]-> (EventTopic:sub) <-[Subscribes]- (subscriber_fn)
//! ```
//!
//! For each `EventTopicMirror` edge `pub_et → sub_et`:
//!   1. Resolve `publisher_fn` via incoming `Publishes` edge on `pub_et`.
//!   2. Resolve `subscriber_fn` via incoming `Subscribes` edge on `sub_et`.
//!   3. Read `canonical_topic` from `pub_et.name` (both nodes share the same
//!      canonical topic string — guaranteed by the same-lib-keyed bucket in
//!      `post_process::event_topic_mirrors`).
//!
//! ## Lib column
//!
//! The `FrameworkId` used to gate same-lib pairing at build time is NOT
//! persisted on archived nodes or edges — it serves only as a bucket key
//! during the post-process pass. The `lib` column is therefore `null` in
//! all outputs. The `--lib` flag is accepted for forward-compatibility but
//! cannot filter on a null value; use `--topic` to narrow by canonical topic
//! string instead. A future schema extension (T5-33-followup) that persists
//! the framework on `EventTopic` nodes will make `--lib` functional.
//!
//! ## Confidence
//!
//! All `EventTopicMirror` edges are emitted with `confidence=0.85` by T5-33
//! (`is_heuristic()` returns `true`). `--min-confidence 0.86` therefore
//! returns 0 rows; `--min-confidence 0.85` returns all.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::graph::{ArchivedNodeKind, ArchivedRelType, ArchivedZeroCopyGraph};
use ecp_core::EcpError;
use serde_json::{json, Value};

/// Args for `ecp find-event-mirrors`.
#[derive(Args, Debug, Clone)]
pub struct FindEventMirrorsArgs {
    /// Filter by framework/transport lib (kafka, redis, rabbitmq, sqs, celery).
    /// Accepted for forward-compatibility; currently no-op because FrameworkId
    /// is not persisted in the archived graph (see module doc).
    #[arg(long, value_name = "LIB")]
    pub lib: Option<String>,

    /// Minimum confidence threshold (default 0.0 — show all mirrors).
    /// T5-33 emits EventTopicMirror edges at confidence=0.85; setting
    /// --min-confidence >0.85 will return 0 rows.
    #[arg(long, value_name = "F", default_value = "0.0")]
    pub min_confidence: f32,

    /// Glob pattern matched against the canonical topic name.
    /// Example: `--topic 'orders/*'` matches `orders/created`, `orders/updated`.
    #[arg(long, value_name = "PATTERN")]
    pub topic: Option<String>,

    /// Output format: text (default) | json | toon
    #[arg(long)]
    pub format: Option<String>,
}

// ── Tier helper ───────────────────────────────────────────────────────────────

fn tier_label(confidence: f32) -> &'static str {
    if confidence >= 0.85 {
        "HEURISTIC"
    } else {
        "BLIND_SPOT"
    }
}

// ── Graph traversal helpers ───────────────────────────────────────────────────

/// Collect (node_idx, confidence) pairs reachable from `node_idx` via
/// outgoing `EventTopicMirror` edges.
fn mirror_targets(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> Vec<(usize, f32)> {
    let out_start = graph.out_offsets[node_idx].to_native() as usize;
    let out_end = graph.out_offsets[node_idx + 1].to_native() as usize;
    (out_start..out_end)
        .filter_map(|i| {
            let edge = &graph.edges[i];
            if matches!(edge.rel_type, ArchivedRelType::EventTopicMirror) {
                Some((
                    edge.target.to_native() as usize,
                    edge.confidence.to_native(),
                ))
            } else {
                None
            }
        })
        .collect()
}

/// Selector for which directional edge to follow when resolving an endpoint.
#[derive(Clone, Copy)]
enum Direction {
    Publish,
    Subscribe,
}

/// Find the first function/method node that has an incoming `Publishes` or
/// `Subscribes` edge pointing at `et_idx` (an `EventTopic` node). Returns
/// `None` when no directional edge was emitted (anonymous callback / lookup
/// miss in the post-process pass).
fn resolve_fn_node(graph: &ArchivedZeroCopyGraph, et_idx: usize, dir: Direction) -> Option<usize> {
    let in_start = graph.in_offsets[et_idx].to_native() as usize;
    let in_end = graph.in_offsets[et_idx + 1].to_native() as usize;
    for i in in_start..in_end {
        let edge_idx = graph.in_edge_idx[i].to_native() as usize;
        let edge = &graph.edges[edge_idx];
        let matches_dir = match dir {
            Direction::Publish => matches!(edge.rel_type, ArchivedRelType::Publishes),
            Direction::Subscribe => matches!(edge.rel_type, ArchivedRelType::Subscribes),
        };
        if matches_dir {
            return Some(edge.source.to_native() as usize);
        }
    }
    None
}

/// Build the JSON object for a function/method endpoint node. Falls back to
/// `null` fields when no directional edge resolved to a concrete function.
fn endpoint_json(graph: &ArchivedZeroCopyGraph, fn_idx_opt: Option<usize>) -> Value {
    match fn_idx_opt {
        None => json!({ "name": null, "file": null, "line": null }),
        Some(fn_idx) => {
            let node = &graph.nodes[fn_idx];
            let name = node.name.resolve(&graph.string_pool);
            let file_node = &graph.files[node.file_idx.to_native() as usize];
            let file = file_node.path.resolve(&graph.string_pool);
            let line = node.span.0.to_native();
            json!({ "name": name, "file": file, "line": line })
        }
    }
}

// ── Text format ───────────────────────────────────────────────────────────────

fn render_text(mirrors: &[Value]) -> String {
    if mirrors.is_empty() {
        return String::from("(no event mirrors found)");
    }
    let col_w = [30usize, 30, 24, 8, 10]; // publisher, subscriber, topic, lib, confidence
    let header = format!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {}",
        "publisher_fn",
        "subscriber_fn",
        "topic",
        "lib",
        "confidence",
        w0 = col_w[0],
        w1 = col_w[1],
        w2 = col_w[2],
        w3 = col_w[3],
    );
    let sep = "-".repeat(header.len());

    let mut lines = vec![header, sep];
    for m in mirrors {
        let pub_name = m["publisher"]["name"].as_str().unwrap_or("?");
        let sub_name = m["subscriber"]["name"].as_str().unwrap_or("?");
        let topic = m["topic"].as_str().unwrap_or("?");
        let lib = m["lib"].as_str().unwrap_or("null");
        let conf = m["confidence"].as_f64().unwrap_or(0.0);
        lines.push(format!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:.2}",
            pub_name,
            sub_name,
            topic,
            lib,
            conf,
            w0 = col_w[0],
            w1 = col_w[1],
            w2 = col_w[2],
            w3 = col_w[3],
        ));
    }
    lines.join("\n")
}

// ── Glob matching ─────────────────────────────────────────────────────────────

/// Minimal glob: supports `*` (any sequence of non-`/` chars) and `**`
/// (any sequence including `/`). No character classes or `?`. Anchored
/// on both ends.
fn glob_matches(pattern: &str, topic: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), topic.as_bytes())
}

fn glob_match_inner(pat: &[u8], s: &[u8]) -> bool {
    match pat.split_first() {
        None => s.is_empty(),
        Some((&b'*', rest)) => {
            // Check for `**`
            if rest.first() == Some(&b'*') {
                let rest2 = &rest[1..];
                // ** matches zero or more chars including /
                for i in 0..=s.len() {
                    if glob_match_inner(rest2, &s[i..]) {
                        return true;
                    }
                }
                false
            } else {
                // * matches zero or more non-/ chars
                for i in 0..=s.len() {
                    if s[..i].contains(&b'/') {
                        break;
                    }
                    if glob_match_inner(rest, &s[i..]) {
                        return true;
                    }
                }
                false
            }
        }
        Some((&ph, rest_p)) => match s.split_first() {
            None => false,
            Some((&sh, rest_s)) => ph == sh && glob_match_inner(rest_p, rest_s),
        },
    }
}

// ── Core detection ────────────────────────────────────────────────────────────

struct MirrorRow {
    pub_fn_idx: Option<usize>,
    sub_fn_idx: Option<usize>,
    topic: String,
    confidence: f32,
}

fn collect_mirrors(
    graph: &ArchivedZeroCopyGraph,
    min_confidence: f32,
    topic_glob: Option<&str>,
    lib_filter: Option<&str>,
) -> Vec<MirrorRow> {
    // Walk all EventTopic nodes and collect outgoing EventTopicMirror edges.
    let mut rows: Vec<MirrorRow> = Vec::new();

    for (et_pub_idx, node) in graph.nodes.iter().enumerate() {
        if !matches!(node.kind, ArchivedNodeKind::EventTopic) {
            continue;
        }

        let topic_name = node.name.resolve(&graph.string_pool);

        // Topic glob filter.
        if let Some(pat) = topic_glob {
            if !glob_matches(pat, topic_name) {
                continue;
            }
        }

        for (et_sub_idx, confidence) in mirror_targets(graph, et_pub_idx) {
            if confidence < min_confidence {
                continue;
            }

            // lib_filter: FrameworkId is not persisted in the archived graph
            // (see module doc). Accept all rows when --lib is set, matching the
            // documented no-op behavior until the schema is extended.
            let _ = lib_filter; // forward-compat placeholder

            let pub_fn_idx = resolve_fn_node(graph, et_pub_idx, Direction::Publish);
            let sub_fn_idx = resolve_fn_node(graph, et_sub_idx, Direction::Subscribe);

            rows.push(MirrorRow {
                pub_fn_idx,
                sub_fn_idx,
                topic: topic_name.to_owned(),
                confidence,
            });
        }
    }

    rows
}

fn row_to_json(graph: &ArchivedZeroCopyGraph, row: &MirrorRow) -> Value {
    json!({
        "publisher": endpoint_json(graph, row.pub_fn_idx),
        "subscriber": endpoint_json(graph, row.sub_fn_idx),
        "topic": row.topic,
        // lib: null — FrameworkId not persisted in archived graph (see module doc).
        "lib": null,
        "confidence": row.confidence,
        "tier": tier_label(row.confidence),
        "requires_verification": true,
    })
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(args: FindEventMirrorsArgs, engine: &Engine) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    let rows = collect_mirrors(
        graph,
        args.min_confidence,
        args.topic.as_deref(),
        args.lib.as_deref(),
    );
    let mirror_count = rows.len();
    let mirrors: Vec<Value> = rows.iter().map(|r| row_to_json(graph, r)).collect();

    // Text format: table to stdout directly (not wrapped in JSON envelope).
    if matches!(format, OutputFormat::Text) {
        println!("{}", render_text(&mirrors));
        return Ok(());
    }

    let result = json!({
        "mirrors": mirrors,
        "summary": {
            "mirror_count": mirror_count,
            "lib_filter": args.lib,
            "topic_filter": args.topic,
            "min_confidence": args.min_confidence,
            "lib_note": "FrameworkId not persisted in archived graph; --lib is a no-op until schema is extended (T5-33-followup)",
        },
    });

    emit(&result, format)
}
