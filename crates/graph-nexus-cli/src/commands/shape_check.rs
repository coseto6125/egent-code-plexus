//! `gnx shape_check` — drift detector for HTTP consumer ↔ Route shape.
//!
//! Iterates every `RelType::Fetches` edge in the graph, parses its
//! `reason` via [`graph_nexus_analyzer::fetch_shape::parse_reason`],
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
use graph_nexus_analyzer::fetch_shape::parse_reason;
use graph_nexus_core::graph::ArchivedRelType;
use graph_nexus_core::GnxError;
use std::collections::HashMap;

/// Detect drift between HTTP consumer access patterns and the Route shapes
/// advertised by the server — surfaces stale or typo'd field accesses.
#[derive(Args, Debug)]
pub struct ShapeCheckArgs {
    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: text (default) | json | toon
    #[arg(long, default_value = "text")]
    pub format: Option<String>,

    /// Filter: only report drift for routes whose path matches this substring.
    /// When None (default), all routes with extracted shape are checked.
    #[arg(long)]
    pub route: Option<String>,
}

fn build_payload(
    args: ShapeCheckArgs,
    engine: &crate::engine::Engine,
) -> Result<serde_json::Value, GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

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

    let mut report_entries: Vec<serde_json::Value> = Vec::new();
    let mut total_fetches: u64 = 0;
    let mut drift_count: u64 = 0;
    let mut matched_count: usize = 0;

    for edge in graph.edges.iter() {
        if !matches!(&edge.rel_type, ArchivedRelType::Fetches) {
            continue;
        }
        total_fetches += 1;

        let reason_str = edge.reason.resolve(&graph.string_pool);
        let Some(parsed) = parse_reason(reason_str) else {
            continue;
        };

        let target_idx = edge.target.to_native();
        let Some((resp_keys, err_keys)) = shapes.get(&target_idx) else {
            continue;
        };

        // Apply route filter if provided.
        if let Some(ref route_filter) = args.route {
            let route_node = &graph.nodes[target_idx as usize];
            let route_name = route_node.name.resolve(&graph.string_pool);
            // Extract path from route name (format: "METHOD /path" or "/path")
            let route_path = route_name.split_once(' ').map(|(_, p)| p).unwrap_or(route_name);
            if !route_path.contains(route_filter) {
                continue;
            }
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
        matched_count += 1;

        let source_idx = edge.source.to_native() as usize;
        let consumer_node = &graph.nodes[source_idx];
        let consumer_uid = consumer_node.uid.resolve(&graph.string_pool);
        let consumer_name = consumer_node.name.resolve(&graph.string_pool);
        let consumer_file = graph.files[consumer_node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool);

        let route_node = &graph.nodes[target_idx as usize];
        let route_uid = route_node.uid.resolve(&graph.string_pool);
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

    // Emit no-match message if route filter was provided but no edges matched.
    if args.route.is_some() && matched_count == 0 {
        eprintln!(
            "No routes match `{}` in the graph.",
            args.route.as_ref().unwrap()
        );
    }

    Ok(serde_json::json!({
        "status": "success",
        "total_fetches": total_fetches,
        "drift_count": drift_count,
        "drift": report_entries,
    }))
}

pub fn run(
    args: ShapeCheckArgs,
    engine: &crate::engine::Engine,
) -> Result<(), graph_nexus_core::GnxError> {
    let format = crate::output::OutputFormat::parse(args.format.as_deref());
    let value = build_payload(args, engine)?;
    let emit_value = match format {
        OutputFormat::Text => {
            let total = value["total_fetches"].as_u64().unwrap_or(0);
            let drift_count = value["drift_count"].as_u64().unwrap_or(0);
            let header = if drift_count == 0 {
                format!("shape_check: {total} Fetches edge(s), 0 drift detected.")
            } else {
                format!("shape_check: {total} Fetches edge(s), {drift_count} with drift")
            };
            let mut lines: Vec<serde_json::Value> = vec![serde_json::Value::String(header)];
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
        OutputFormat::Json | OutputFormat::Toon => value,
    };
    emit(&emit_value, format)
}
