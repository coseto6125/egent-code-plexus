//! `ecp routes [<path>]` — unified HTTP route command.
//!
//! Without `<path>`: lists all Route nodes (replaces `ecp route_map`).
//! With `<path>`:    shows the handler + full upstream caller chain
//!                   (replaces `ecp api_impact --route <path>`).
//!
//! Optional `--method GET/POST/...` narrows results in both modes.

use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::graph::{ArchivedFileCategory, ArchivedNodeKind, ArchivedRelType};
use ecp_core::EcpError;
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

    /// Include routes declared inside test files (`tests/`, `test/`, `*_test.*`,
    /// `*.spec.*`, etc.). Default: off — most agent queries want production
    /// routes, not test fixtures. When set, the output gains a `test_results`
    /// array listing the test-only routes alongside the regular `results`.
    /// Test classification reuses `File.category = FileCategory::Test` set at
    /// index time (`ecp-analyzer/src/resolution/builder.rs:32`).
    #[arg(long)]
    pub include_tests: bool,

    /// Output format (toon / json / text).
    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: RoutesArgs, engine: &Engine) -> Result<(), EcpError> {
    match args.path.as_deref() {
        None => list_routes(
            engine,
            args.method.as_deref(),
            args.include_tests,
            args.format.as_deref(),
        ),
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
    include_tests: bool,
    format: Option<&str>,
) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let fmt = OutputFormat::parse(format);
    let wanted_method = method_filter.map(|m| m.to_ascii_uppercase());

    let mut results = Vec::new();
    let mut test_results = Vec::new();

    // v10 kind_offsets CSR: iterate Route nodes only, not all N nodes.
    for idx in graph.nodes_by_kind(ecp_core::graph::NodeKind::Route) {
        let node = &graph.nodes[idx as usize];
        let name = node.name.resolve(&graph.string_pool);
        let (method, path) = split_route_name(name);

        if let Some(ref want) = wanted_method {
            if method != want {
                continue;
            }
        }

        let file_node = &graph.files[node.file_idx.to_native() as usize];
        // `Example` intentionally falls through as production: framework
        // example apps (Express `examples/auth/`, Flask `examples/tutorial/`)
        // are canonical "how to wire routes" content LLM consumers want to
        // see. Only `Test` flips `is_test` here, matching the builder.rs
        // route-emission skip filter which gates on `Test | Reference`.
        let is_test = matches!(file_node.category, ArchivedFileCategory::Test);
        let row = serde_json::json!({
            "uid": node.uid.to_native().to_string(),
            "method": method,
            "path": path,
            "kind": "Route",
            "filePath": file_node.path.resolve(&graph.string_pool),
            "line": node.span.0.to_native(),
        });
        match (is_test, include_tests) {
            (false, _) => results.push(row),
            (true, true) => test_results.push(row),
            (true, false) => {} // silently dropped — that's the default
        }
    }

    if results.is_empty() && test_results.is_empty() {
        eprintln!(
            "No HTTP routes detected.\n\
             → Possible causes: framework not yet supported, no route declarations found,\n\
             or a coverage gap. Run `ecp summary --detailed` for framework scan details."
        );
    } else if results.is_empty() && !include_tests {
        eprintln!(
            "No production routes detected ({} test-file routes were filtered).\n\
             → Re-run with `--include-tests` to inspect them.",
            graph
                .nodes_by_kind(ecp_core::graph::NodeKind::Route)
                .filter(|&idx| {
                    let n = &graph.nodes[idx as usize];
                    matches!(
                        graph.files[n.file_idx.to_native() as usize].category,
                        ArchivedFileCategory::Test
                    )
                })
                .count()
        );
    }

    let mut result = serde_json::json!({
        "status": "success",
        "method_filter": wanted_method,
        "results": results,
    });
    if include_tests {
        result["test_results"] = serde_json::json!(test_results);
    }

    emit(&result, fmt)
}

/// Split a Route node's `name` field — stored as `"<METHOD> <path>"` — into
/// `(method, path)`. Falls back to `("", name)` for defensive handling.
fn split_route_name(name: &str) -> (&str, &str) {
    name.split_once(' ').unwrap_or(("", name))
}

/// Find the smallest Function/Method/Constructor/Class node in the same
/// file whose line span encloses `line`. Returns the node index, or `None`
/// for module-level routes with no enclosing scope. Multi-line `Const`
/// nodes (arrow-function bound to a const, e.g. `const setup = () => {…}`)
/// are also considered — TS / JS commonly express scope this way.
fn find_enclosing_scope(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    file_idx: u32,
    line: u32,
) -> Option<usize> {
    let mut best: Option<(u32, usize)> = None;
    for (i, node) in graph.nodes.iter().enumerate() {
        if node.file_idx.to_native() != file_idx {
            continue;
        }
        let is_scope_kind = matches!(
            &node.kind,
            ArchivedNodeKind::Function
                | ArchivedNodeKind::Method
                | ArchivedNodeKind::Constructor
                | ArchivedNodeKind::Class
                | ArchivedNodeKind::Const
        );
        if !is_scope_kind {
            continue;
        }
        let start_line = node.span.0.to_native();
        let end_line = node.span.2.to_native();
        // Strict containment: a route registered at the same line as the
        // function's signature shouldn't be claimed by an unrelated sibling.
        // Inclusive bounds are correct because the function's span covers
        // its signature line through its closing brace.
        if start_line > line || end_line < line {
            continue;
        }
        // Const that spans a single line is a plain value binding, not a
        // scope — skip to avoid matching `const x = 1;` against the route's
        // own line (which would otherwise be a perfect zero-size match).
        if matches!(&node.kind, ArchivedNodeKind::Const) && start_line == end_line {
            continue;
        }
        let span_size = end_line.saturating_sub(start_line);
        match best {
            None => best = Some((span_size, i)),
            Some((cur, _)) if span_size < cur => best = Some((span_size, i)),
            _ => {}
        }
    }
    best.map(|(_, i)| i)
}

/// Build the `enclosingScope` JSON sub-object for a Route. Returns `null`
/// when the route sits at module level (no containing function/class).
fn enclosing_scope_json(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    scope_idx: Option<usize>,
) -> serde_json::Value {
    match scope_idx {
        Some(idx) => {
            let n = &graph.nodes[idx];
            serde_json::json!({
                "uid": n.uid.to_native().to_string(),
                "name": n.name.resolve(&graph.string_pool),
                "kind": kind_to_str(&n.kind),
                "line": n.span.0.to_native(),
            })
        }
        None => serde_json::Value::Null,
    }
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
) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let fmt = OutputFormat::parse(format);
    let wanted_method = method_filter.map(|m| m.to_ascii_uppercase());

    // Step 1: find Route node(s) matching path (and optional method).
    let mut matched: Vec<usize> = Vec::new();
    let mut all_routes: Vec<(usize, String, String)> = Vec::new(); // (idx, method, path)

    for i_u32 in graph.nodes_by_kind(ecp_core::graph::NodeKind::Route) {
        let i = i_u32 as usize;
        let node = &graph.nodes[i];
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
        let total_fuzzy_candidates = scored.len() as u64;
        let zero_score_omitted = scored.iter().filter(|(s, _)| *s == 0.0).count() as u64;
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
                    "uid": node.uid.to_native().to_string(),
                    "filePath": file_node.path.resolve(&graph.string_pool),
                })
            })
            .collect();
        let shown = candidates.len() as u64;
        let omitted = total_fuzzy_candidates - shown;

        if matches!(fmt, OutputFormat::Text) && omitted > 0 {
            eprintln!(
                "note: {omitted} fuzzy candidate(s) omitted (shown {shown} of {total_fuzzy_candidates})"
            );
        }

        let result = serde_json::json!({
            "status": "not_found",
            "route_pattern": wanted_path,
            "method": wanted_method,
            "candidates": candidates,
            "total_fuzzy_candidates": total_fuzzy_candidates,
            "shown": shown,
            "zero_score_omitted": zero_score_omitted,
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
        let route_file_idx = route_node.file_idx.to_native();
        let route_file = &graph.files[route_file_idx as usize];
        let route_line = route_node.span.0.to_native();
        let route_file_path = route_file.path.resolve(&graph.string_pool);

        // Smallest containing scope of the route registration call. Used
        // for both the per-route `enclosingScope` field and as the BFS seed
        // for inline-anonymous handlers (which have no node of their own).
        let enclosing_idx = find_enclosing_scope(graph, route_file_idx, route_line);

        routes_out.push(serde_json::json!({
            "method": route_method,
            "path": route_path,
            "uid": route_node.uid.to_native().to_string(),
            "filePath": route_file_path,
            "line": route_line,
            "enclosingScope": enclosing_scope_json(graph, enclosing_idx),
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

        // Inline-anonymous fallback: no HandlesRoute edge means the
        // registration was `app.get(path, (req, res) => …)` — the parser
        // intentionally does not synthesize a Node for an anonymous arrow.
        // Surface this explicitly instead of returning an empty array, and
        // seed the upstream BFS from the enclosing scope so consumers still
        // get meaningful caller context (who wires up this route).
        if handler_indices.is_empty() {
            handlers_out.push(serde_json::json!({
                "uid": serde_json::Value::Null,
                "name": "<inline>",
                "kind": "InlineHandler",
                "handlerKind": "inline_anonymous",
                "filePath": route_file_path,
                "line": route_line,
                "route": route_name,
                "enclosingScope": enclosing_scope_json(graph, enclosing_idx),
            }));
            if let Some(scope_idx) = enclosing_idx {
                bfs_upstream(
                    graph,
                    scope_idx,
                    depth,
                    &route_node.uid.to_native().to_string(),
                    &mut callers_out,
                );
            }
            continue;
        }

        for handler_idx in handler_indices {
            if !seen_handlers.insert(handler_idx) {
                continue;
            }
            let handler_node = &graph.nodes[handler_idx];
            if !handler_node.has_owning_file() {
                continue;
            }
            let handler_file = &graph.files[handler_node.file_idx.to_native() as usize];

            handlers_out.push(serde_json::json!({
                "uid": handler_node.uid.to_native().to_string(),
                "name": handler_node.name.resolve(&graph.string_pool),
                "kind": kind_to_str(&handler_node.kind),
                "handlerKind": "named",
                "filePath": handler_file.path.resolve(&graph.string_pool),
                "line": handler_node.span.0.to_native(),
                "route": route_name,
                "enclosingScope": enclosing_scope_json(graph, enclosing_idx),
            }));

            bfs_upstream(
                graph,
                handler_idx,
                depth,
                &handler_node.uid.to_native().to_string(),
                &mut callers_out,
            );
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

/// BFS upstream over non-`HandlesRoute` incoming edges from `seed_idx`,
/// pushing every reached node (except the seed itself) to `out` with
/// depth + via-edge attribution. `anchor_uid` is recorded on each caller
/// entry as `handlerUid` so consumers can group callers per registration
/// site even when multiple are surfaced together.
fn bfs_upstream(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    seed_idx: usize,
    depth: usize,
    anchor_uid: &str,
    out: &mut Vec<serde_json::Value>,
) {
    type ViaEdge = Option<(String, f32)>;
    type Step = (usize, usize, ViaEdge);
    let mut visited: HashSet<usize> = HashSet::new();
    let mut queue: VecDeque<Step> = VecDeque::new();
    queue.push_back((seed_idx, 0, None));
    visited.insert(seed_idx);

    while let Some((curr_idx, curr_depth, via)) = queue.pop_front() {
        if curr_idx != seed_idx {
            let curr_node = &graph.nodes[curr_idx];
            if !curr_node.has_owning_file() {
                continue;
            }
            let file_node = &graph.files[curr_node.file_idx.to_native() as usize];
            let (via_reason, via_confidence) = via
                .as_ref()
                .map(|(r, c)| (r.as_str(), *c))
                .unwrap_or(("", 1.0));
            out.push(serde_json::json!({
                "uid": curr_node.uid.to_native().to_string(),
                "name": curr_node.name.resolve(&graph.string_pool),
                "kind": kind_to_str(&curr_node.kind),
                "filePath": file_node.path.resolve(&graph.string_pool),
                "line": curr_node.span.0.to_native(),
                "depth": curr_depth,
                "viaReason": via_reason,
                "viaConfidence": via_confidence,
                "handlerUid": anchor_uid,
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
            // Skip HandlesRoute — entry point, not an upstream caller.
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
