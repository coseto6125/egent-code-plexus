//! `ecp shape_check` — drift detector for HTTP consumer ↔ Route shape.
//!
//! Iterates every `RelType::Fetches` edge in the graph, parses its
//! `reason` via [`ecp_analyzer::fetch_shape::parse_reason`],
//! looks up the target Route's `RouteShape`, and reports any
//! consumer-accessed key that is NOT in `response_keys ∪ error_keys`.
//!
//! A "drift" row means the consumer reads a field the server doesn't
//! advertise — either the server changed its payload shape, the
//! consumer extracts a typo'd / stale key, or the response-shape
//! extractor missed a branch. All three are worth flagging to the LLM
//! that's about to change either side.
//!
//! Edges whose target Route has no extracted `RouteShape` are skipped
//! silently — we can't compare against an empty known-set without
//! flooding the report with false positives (every key would drift).
//! Edges whose reason doesn't parse as a Fetches reason are also
//! skipped; `parse_reason` returning `None` is the documented contract.
//!
//! Output is human-readable text by default (this is a drift report
//! agents read inline). JSON / TOON are for programmatic consumers.

use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_analyzer::fetch_shape::parse_reason;
use ecp_core::graph::ArchivedRelType;
use ecp_core::EcpError;
use std::collections::HashMap;

/// Detect drift between HTTP consumer access patterns and the Route shapes
/// advertised by the server — surfaces stale or typo'd field accesses.
#[derive(Args, Debug)]
pub struct ShapeCheckArgs {
    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: text (default) | json | toon
    #[arg(long)]
    pub format: Option<String>,

    /// Filter: only report drift for routes whose path matches this substring.
    /// When None (default), all routes with extracted shape are checked.
    #[arg(long)]
    pub route: Option<String>,
}

#[derive(Default)]
struct ShapeCheckHints {
    /// Set when `--route <filter>` was given but no Fetches edge resolved
    /// to a Route whose path matched. CLI emits this to stderr from `run`;
    /// library callers ignore it.
    unmatched_route_filter: Option<String>,
    /// Silent-drop counters surfaced verbatim by `render_text` — typed copies
    /// of the JSON payload fields so the text renderer doesn't need to
    /// reach back into the serialized value via `.as_u64() / .as_bool()`.
    unparseable_fetches: u64,
    unknown_target_shapes_total: u64,
    unknown_target_shapes_truncated: bool,
}

/// Library API: returns the JSON payload only, dropping stderr hints.
///
/// `run` (binary path) calls `build_payload_with_hints` directly so it can
/// print the `unmatched_route_filter` hint to stderr, which means this thin
/// wrapper has no in-crate caller and `cargo` flags it as dead. Kept `pub` to
/// mirror the 5-command `build_payload` surface introduced in PR #88 for
/// future library consumers.
#[allow(dead_code)]
pub fn build_payload(
    args: &ShapeCheckArgs,
    engine: &crate::engine::Engine,
) -> Result<serde_json::Value, EcpError> {
    build_payload_with_hints(args, engine).map(|(v, _)| v)
}

fn build_payload_with_hints(
    args: &ShapeCheckArgs,
    engine: &crate::engine::Engine,
) -> Result<(serde_json::Value, ShapeCheckHints), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;

    // Lookup: route node_idx → (known_keys set, response_keys list, error_keys list).
    // One pass over route_shapes; resolves StrRef → owned String once so the
    // edge-scan loop stays a pure compare.
    let shapes: HashMap<u32, (Vec<String>, Vec<String>)> = graph
        .route_shapes
        .iter()
        .map(|s| {
            let resp: Vec<String> = s
                .response_keys
                .iter()
                .map(|k| k.resolve(&graph.string_pool).to_string())
                .collect();
            let err: Vec<String> = s
                .error_keys
                .iter()
                .map(|k| k.resolve(&graph.string_pool).to_string())
                .collect();
            (s.node_idx.to_native(), (resp, err))
        })
        .collect();

    // Cap the unknown-shape list so a microservice repo where most Fetches
    // point at external endpoints doesn't balloon the JSON payload (and the
    // LLM consumer's token budget) with thousands of tiny Maps. Beyond the
    // cap we still count for the truncated flag.
    const UNKNOWN_TARGET_SHAPES_CAP: usize = 50;

    let mut report_entries: Vec<serde_json::Value> = Vec::new();
    let mut total_fetches: u64 = 0;
    let mut drift_count: u64 = 0;
    let mut filter_matched_count: usize = 0;
    let mut unparseable_fetches: u64 = 0;
    let mut unknown_target_shapes: Vec<serde_json::Value> = Vec::new();
    let mut unknown_target_shapes_total: u64 = 0;

    for edge in graph.edges.iter() {
        if !matches!(&edge.rel_type, ArchivedRelType::Fetches) {
            continue;
        }
        total_fetches += 1;

        let reason_str = edge.reason.resolve(&graph.string_pool);
        let Some(parsed) = parse_reason(reason_str) else {
            unparseable_fetches += 1;
            continue;
        };

        let target_idx = edge.target.to_native();
        let Some((resp_keys, err_keys)) = shapes.get(&target_idx) else {
            // Surface "there's a Fetches edge here but the target has no
            // ResponseShape/ErrorShape on file" instead of dropping silently.
            // LLMs need to distinguish "no drift" from "couldn't check".
            unknown_target_shapes_total += 1;
            if unknown_target_shapes.len() < UNKNOWN_TARGET_SHAPES_CAP {
                let route_node = &graph.nodes[target_idx as usize];
                unknown_target_shapes.push(serde_json::json!({
                    "route_uid": route_node.uid.to_native().to_string(),
                    "route_name": route_node.name.resolve(&graph.string_pool),
                }));
            }
            continue;
        };

        // Apply route filter if provided.
        if let Some(ref route_filter) = args.route {
            let route_node = &graph.nodes[target_idx as usize];
            let route_name = route_node.name.resolve(&graph.string_pool);
            // Extract path from route name (format: "METHOD /path" or "/path")
            let route_path = route_name
                .split_once(' ')
                .map(|(_, p)| p)
                .unwrap_or(route_name);
            if !route_path.contains(route_filter) {
                continue;
            }
            filter_matched_count += 1;
        }

        // `known` is response_keys ∪ error_keys. We iterate parsed.keys and
        // collect any that aren't present.
        let drift: Vec<&String> = parsed
            .keys
            .iter()
            .filter(|k| !resp_keys.contains(k) && !err_keys.contains(k))
            .collect();

        if drift.is_empty() {
            continue;
        }
        drift_count += 1;

        let source_idx = edge.source.to_native() as usize;
        let consumer_node = &graph.nodes[source_idx];
        if !consumer_node.has_owning_file() {
            continue;
        }
        let consumer_uid_str = consumer_node.uid.to_native().to_string();
        let consumer_uid = consumer_uid_str.as_str();
        let consumer_name = consumer_node.name.resolve(&graph.string_pool);
        let consumer_file = graph.files[consumer_node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool);

        let route_node = &graph.nodes[target_idx as usize];
        let route_uid_str = route_node.uid.to_native().to_string();
        let route_uid = route_uid_str.as_str();
        let route_name = route_node.name.resolve(&graph.string_pool);

        let drift_owned: Vec<String> = drift.iter().map(|&s| s.clone()).collect();

        report_entries.push(serde_json::json!({
            "consumer_uid": consumer_uid,
            "consumer_name": consumer_name,
            "consumer_file": consumer_file,
            "route_uid": route_uid,
            "route_name": route_name,
            "drift_keys": drift_owned,
            "response_keys": resp_keys,
            "error_keys": err_keys,
            "fetch_count": parsed.fetch_count,
        }));
    }

    let unknown_target_shapes_truncated =
        unknown_target_shapes_total > unknown_target_shapes.len() as u64;
    let hints = ShapeCheckHints {
        unmatched_route_filter: args
            .route
            .as_ref()
            .filter(|_| filter_matched_count == 0)
            .cloned(),
        unparseable_fetches,
        unknown_target_shapes_total,
        unknown_target_shapes_truncated,
    };
    let value = serde_json::json!({
        "status": "success",
        "total_fetches": total_fetches,
        "drift_count": drift_count,
        "drift": report_entries,
        // Always-written silent-drop signals (safe defaults 0 / []).
        "unparseable_fetches": unparseable_fetches,
        "unknown_target_shapes": unknown_target_shapes,
        // True iff the cap kicked in — LLM should treat the list as a sample.
        "unknown_target_shapes_truncated": unknown_target_shapes_truncated,
        "unknown_target_shapes_total": unknown_target_shapes_total,
    });
    Ok((value, hints))
}

fn render_text(value: &serde_json::Value, hints: &ShapeCheckHints) -> serde_json::Value {
    let total = value["total_fetches"].as_u64().unwrap_or(0);
    let drift_count = value["drift_count"].as_u64().unwrap_or(0);
    let header = if drift_count == 0 {
        format!("shape_check: {total} Fetches edge(s), 0 drift detected.")
    } else {
        format!("shape_check: {total} Fetches edge(s), {drift_count} with drift")
    };
    let mut lines: Vec<serde_json::Value> = vec![serde_json::Value::String(header)];
    if hints.unparseable_fetches > 0 || hints.unknown_target_shapes_total > 0 {
        let trunc_note = if hints.unknown_target_shapes_truncated {
            " (list truncated)"
        } else {
            ""
        };
        let unparseable = hints.unparseable_fetches;
        let unknown_shapes = hints.unknown_target_shapes_total;
        lines.push(serde_json::Value::String(format!(
            "note: {unparseable} unparseable Fetches reason(s), {unknown_shapes} route(s) with no known ResponseShape{trunc_note} — drift was NOT checked for these"
        )));
    }
    if let Some(drift) = value["drift"].as_array() {
        if !drift.is_empty() {
            lines.push(serde_json::Value::String(String::new()));
            for entry in drift {
                let consumer_file = entry["consumer_file"].as_str().unwrap_or("");
                let consumer_name = entry["consumer_name"].as_str().unwrap_or("");
                let route_name = entry["route_name"].as_str().unwrap_or("");
                let drift_keys = entry["drift_keys"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let resp_keys = entry["response_keys"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let err_keys = entry["error_keys"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                lines.push(serde_json::Value::String(format!(
                    "DRIFT  {consumer_file}:{consumer_name}  →  {route_name}\n       consumer reads:  {drift_keys:?}\n       route emits:     response_keys={resp_keys:?} error_keys={err_keys:?}"
                )));
            }
        }
    }
    serde_json::json!({ "results": lines })
}

pub fn run(args: ShapeCheckArgs, engine: &crate::engine::Engine) -> Result<(), ecp_core::EcpError> {
    let format = crate::output::OutputFormat::parse(args.format.as_deref());
    let (value, hints) = build_payload_with_hints(&args, engine)?;
    if let Some(filter) = &hints.unmatched_route_filter {
        eprintln!("No routes match `{filter}` in the graph.");
    }
    let emit_value = if matches!(format, OutputFormat::Text) {
        render_text(&value, &hints)
    } else {
        value
    };
    emit(&emit_value, format)
}
