//! `gnx routes [<path>]` — unified HTTP route command.
//!
//! Without `<path>`: lists all Route nodes (replaces `gnx route_map`).
//! With `<path>`:    shows the handler + full upstream caller chain
//!                   (replaces `gnx api_impact --route <path>`).
//!
//! Optional `--method GET/POST/...` narrows results in both modes.

use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::graph::{ArchivedNodeKind, ArchivedRelType};
use graph_nexus_core::GnxError;
use std::collections::{HashSet, VecDeque};

#[derive(Args, Debug, Clone)]
pub struct RoutesArgs {
    /// If given, show handler + caller chain for this route path.
    /// If omitted, list all routes.
    pub path: Option<String>,

    /// Filter by HTTP method (GET / POST / PATCH / DELETE / ...).
    #[arg(long)]
    pub method: Option<String>,

    /// Repository selector.
    #[arg(long)]
    pub repo: Option<String>,

    /// Max depth for upstream caller traversal (only applies when <path> is set).
    #[arg(long, default_value = "3")]
    pub depth: usize,

    /// Output format (toon / json / text).
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,
}

pub fn run(args: RoutesArgs, engine: &Engine) -> Result<(), GnxError> {
    match args.path.as_deref() {
        None => list_routes(engine, args.method.as_deref(), args.format.as_deref()),
        Some(path) => inspect_route(
            engine,
            path,
            args.method.as_deref(),
            args.depth,
            args.format.as_deref(),
        ),
    }
}

fn list_routes(
    engine: &Engine,
    method_filter: Option<&str>,
    format: Option<&str>,
) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let fmt = OutputFormat::parse(format);
    let wanted_method = method_filter.map(|m| m.to_ascii_uppercase());

    let mut results = Vec::new();

    for node in graph.nodes.iter() {
        if !matches!(&node.kind, ArchivedNodeKind::Route) {
            continue;
        }
        let name = node.name.resolve(&graph.string_pool);
        let (method, path) = split_route_name(name);

        if let Some(ref want) = wanted_method {
            if method != want {
                continue;
            }
        }

        let file_node = &graph.files[node.file_idx.to_native() as usize];
        results.push(serde_json::json!({
            "uid": node.uid.resolve(&graph.string_pool),
            "method": method,
            "path": path,
            "kind": "Route",
            "filePath": file_node.path.resolve(&graph.string_pool),
            "line": node.span.0.to_native(),
        }));
    }

    if results.is_empty() {
        eprintln!(
            "No HTTP routes detected.\n\
             → Possible causes: framework not yet supported, no route declarations found,\n\
             or a coverage gap. Run `gnx coverage --detailed` for framework scan details."
        );
    }

    let result = serde_json::json!({
        "status": "success",
        "method_filter": wanted_method,
        "results": results,
    });

    emit(&result, fmt)
}

/// Split a Route node's `name` field — stored as `"<METHOD> <path>"` — into
/// `(method, path)`. Falls back to `("", name)` for defensive handling.
fn split_route_name(name: &str) -> (&str, &str) {
    name.split_once(' ').unwrap_or(("", name))
}

/// Cheap path-similarity score in [0, 1] for the not-found fallback.
fn similarity(a: &str, b: &str) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let common = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let max_len = a.len().max(b.len()) as f32;
    common as f32 / max_len
}

fn inspect_route(
    engine: &Engine,
    wanted_path: &str,
    method_filter: Option<&str>,
    depth: usize,
    format: Option<&str>,
) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let fmt = OutputFormat::parse(format);
    let wanted_method = method_filter.map(|m| m.to_ascii_uppercase());

    // Step 1: find Route node(s) matching path (and optional method).
    let mut matched: Vec<usize> = Vec::new();
    let mut all_routes: Vec<(usize, String, String)> = Vec::new(); // (idx, method, path)

    for (i, node) in graph.nodes.iter().enumerate() {
        if !matches!(&node.kind, ArchivedNodeKind::Route) {
            continue;
        }
        let name = node.name.resolve(&graph.string_pool);
        let (method, path) = split_route_name(name);
        all_routes.push((i, method.to_string(), path.to_string()));

        if path != wanted_path {
            continue;
        }
        if let Some(ref want) = wanted_method {
            if method != want {
                continue;
            }
        }
        matched.push(i);
    }

    if matched.is_empty() {
        let mut scored: Vec<(f32, &(usize, String, String))> = all_routes
            .iter()
            .map(|r| (similarity(wanted_path, &r.2), r))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let candidates: Vec<serde_json::Value> = scored
            .iter()
            .take(5)
            .filter(|(score, _)| *score > 0.0)
            .map(|(_, (idx, method, path))| {
                let node = &graph.nodes[*idx];
                let file_node = &graph.files[node.file_idx.to_native() as usize];
                serde_json::json!({
                    "method": method,
                    "path": path,
                    "uid": node.uid.resolve(&graph.string_pool),
                    "filePath": file_node.path.resolve(&graph.string_pool),
                })
            })
            .collect();

        let result = serde_json::json!({
            "status": "not_found",
            "route_pattern": wanted_path,
            "method": wanted_method,
            "candidates": candidates,
        });
        return emit(&result, fmt);
    }

    // For each matched Route: find handler via incoming HandlesRoute edge,
    // then BFS upstream from handler.
    let mut routes_out = Vec::with_capacity(matched.len());
    let mut handlers_out = Vec::new();
    let mut callers_out = Vec::new();
    let mut seen_handlers: HashSet<usize> = HashSet::new();

    for route_idx in matched {
        let route_node = &graph.nodes[route_idx];
        let route_name = route_node.name.resolve(&graph.string_pool);
        let (route_method, route_path) = split_route_name(route_name);
        let route_file = &graph.files[route_node.file_idx.to_native() as usize];

        routes_out.push(serde_json::json!({
            "method": route_method,
            "path": route_path,
            "uid": route_node.uid.resolve(&graph.string_pool),
            "filePath": route_file.path.resolve(&graph.string_pool),
            "line": route_node.span.0.to_native(),
        }));

        // Handler: incoming HandlesRoute edge on the Route node.
        let in_start = graph.in_offsets[route_idx].to_native() as usize;
        let in_end = graph.in_offsets[route_idx + 1].to_native() as usize;
        let mut handler_indices: Vec<usize> = Vec::new();
        for i in in_start..in_end {
            let edge_idx = graph.in_edge_idx[i].to_native() as usize;
            let edge = &graph.edges[edge_idx];
            if matches!(&edge.rel_type, ArchivedRelType::HandlesRoute) {
                handler_indices.push(edge.source.to_native() as usize);
            }
        }

        for handler_idx in handler_indices {
            if !seen_handlers.insert(handler_idx) {
                continue;
            }
            let handler_node = &graph.nodes[handler_idx];
            let handler_file = &graph.files[handler_node.file_idx.to_native() as usize];

            handlers_out.push(serde_json::json!({
                "uid": handler_node.uid.resolve(&graph.string_pool),
                "name": handler_node.name.resolve(&graph.string_pool),
                "kind": kind_to_str(&handler_node.kind),
                "filePath": handler_file.path.resolve(&graph.string_pool),
                "line": handler_node.span.0.to_native(),
                "route": route_name,
            }));

            // BFS upstream from the handler.
            type ViaEdge = Option<(String, f32)>;
            type Step = (usize, usize, ViaEdge);
            let mut visited: HashSet<usize> = HashSet::new();
            let mut queue: VecDeque<Step> = VecDeque::new();
            queue.push_back((handler_idx, 0, None));
            visited.insert(handler_idx);

            while let Some((curr_idx, curr_depth, via)) = queue.pop_front() {
                // Skip the handler itself — it's already in `handlers_out`.
                if curr_idx != handler_idx {
                    let curr_node = &graph.nodes[curr_idx];
                    let file_node = &graph.files[curr_node.file_idx.to_native() as usize];
                    let (via_reason, via_confidence) = via
                        .as_ref()
                        .map(|(r, c)| (r.as_str(), *c))
                        .unwrap_or(("", 1.0));
                    callers_out.push(serde_json::json!({
                        "uid": curr_node.uid.resolve(&graph.string_pool),
                        "name": curr_node.name.resolve(&graph.string_pool),
                        "kind": kind_to_str(&curr_node.kind),
                        "filePath": file_node.path.resolve(&graph.string_pool),
                        "line": curr_node.span.0.to_native(),
                        "depth": curr_depth,
                        "viaReason": via_reason,
                        "viaConfidence": via_confidence,
                        "handlerUid": handler_node.uid.resolve(&graph.string_pool),
                    }));
                }

                if curr_depth >= depth {
                    continue;
                }

                let in_start = graph.in_offsets[curr_idx].to_native() as usize;
                let in_end = graph.in_offsets[curr_idx + 1].to_native() as usize;
                for i in in_start..in_end {
                    let edge_idx = graph.in_edge_idx[i].to_native() as usize;
                    let edge = &graph.edges[edge_idx];
                    // Skip HandlesRoute — that's the entry point, not an upstream caller.
                    if matches!(&edge.rel_type, ArchivedRelType::HandlesRoute) {
                        continue;
                    }
                    let next_idx = edge.source.to_native() as usize;
                    if visited.insert(next_idx) {
                        let edge_reason = edge.reason.resolve(&graph.string_pool).to_string();
                        queue.push_back((
                            next_idx,
                            curr_depth + 1,
                            Some((edge_reason, edge.confidence.to_native())),
                        ));
                    }
                }
            }
        }
    }

    let result = serde_json::json!({
        "status": "found",
        "route_pattern": wanted_path,
        "method": wanted_method,
        "routes": routes_out,
        "handlers": handlers_out,
        "callers": callers_out,
    });
    emit(&result, fmt)
}
