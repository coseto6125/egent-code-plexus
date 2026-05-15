use crate::auto_ensure::{ensure_index, EnsureResult};
use crate::commands::format::{kind_to_str, rel_to_str};
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::algorithms::process_trace::is_test_path;
use graph_nexus_core::graph::ArchivedZeroCopyGraph;
use graph_nexus_core::GnxError;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// Name of the symbol to inspect.
    #[arg(long)]
    pub name: Option<String>,

    /// Repository path
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,

    /// Comma-separated list of node kinds (lowercase, e.g. `function,method`)
    /// to keep on the *target* side of incoming/outgoing edges.
    #[arg(long)]
    pub kind: Option<String>,

    /// Substring filter applied to the target file path of incoming/outgoing
    /// edges. Case-sensitive substring match (not glob).
    #[arg(long = "file_path", alias = "file-path")]
    pub file_path: Option<String>,

    /// Comma-separated list of relation types (lowercase, e.g. `calls,imports`).
    #[arg(long = "relation_types", alias = "relation-types")]
    pub relation_types: Option<String>,

    /// Include edges whose target lives in a test file. Defaults to false.
    #[arg(
        long = "include_tests",
        alias = "include-tests",
        alias = "includeTests",
        default_value_t = false
    )]
    pub include_tests: bool,
}

/// Split a `a,b,c` style value into a lower-cased Vec. Trims whitespace and
/// drops empty segments. `None` / empty input → no filter.
fn parse_csv_lower(s: Option<&str>) -> Option<Vec<String>> {
    let raw = s?;
    let parts: Vec<String> = raw
        .split(',')
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

/// Build the full inspect payload for a single node index.
///
/// Returns a JSON object with: symbol (no uid), incoming, outgoing,
/// blind_spots, and impact_upstream_1hop.
fn build_inspect_block(
    graph: &ArchivedZeroCopyGraph,
    node_idx: usize,
    kind_filter: &Option<Vec<String>>,
    rel_filter: &Option<Vec<String>>,
    file_substr: Option<&str>,
    include_tests: bool,
) -> serde_json::Value {
    let node = &graph.nodes[node_idx];
    let file_node = &graph.files[node.file_idx.to_native() as usize];
    let file_path_str = file_node.path.resolve(&graph.string_pool);

    let edge_keeps = |target_kind_str: &str, target_file_path: &str, rel_str: &str| -> bool {
        if let Some(ref kinds) = kind_filter {
            if !kinds
                .iter()
                .any(|k| k == &target_kind_str.to_ascii_lowercase())
            {
                return false;
            }
        }
        if let Some(ref rels) = rel_filter {
            if !rels.iter().any(|r| r == rel_str) {
                return false;
            }
        }
        if let Some(substr) = file_substr {
            if !target_file_path.contains(substr) {
                return false;
            }
        }
        if !include_tests && is_test_path(target_file_path) {
            return false;
        }
        true
    };

    let mut incoming: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut outgoing: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

    // Outgoing edges
    let out_start = graph.out_offsets[node_idx].to_native() as usize;
    let out_end = graph.out_offsets[node_idx + 1].to_native() as usize;
    for i in out_start..out_end {
        let edge = &graph.edges[i];
        let target_node = &graph.nodes[edge.target.to_native() as usize];
        let target_file = &graph.files[target_node.file_idx.to_native() as usize];
        let target_file_path = target_file.path.resolve(&graph.string_pool);
        let target_kind = kind_to_str(&target_node.kind);
        let rel_str = rel_to_str(&edge.rel_type).to_string();

        if !edge_keeps(target_kind, target_file_path, &rel_str) {
            continue;
        }

        let entry = serde_json::json!({
            "name": target_node.name.resolve(&graph.string_pool),
            "kind": target_kind,
            "filePath": target_file_path,
            "reason": edge.reason.resolve(&graph.string_pool),
            "confidence": edge.confidence.to_native(),
        });
        outgoing.entry(rel_str).or_default().push(entry);
    }

    // Incoming edges
    let in_start = graph.in_offsets[node_idx].to_native() as usize;
    let in_end = graph.in_offsets[node_idx + 1].to_native() as usize;
    for i in in_start..in_end {
        let edge_idx = graph.in_edge_idx[i].to_native() as usize;
        let edge = &graph.edges[edge_idx];
        let source_node = &graph.nodes[edge.source.to_native() as usize];
        let source_file = &graph.files[source_node.file_idx.to_native() as usize];
        let source_file_path = source_file.path.resolve(&graph.string_pool);
        let source_kind = kind_to_str(&source_node.kind);
        let rel_str = rel_to_str(&edge.rel_type).to_string();

        // For incoming edges the "target" we filter against is the OTHER end —
        // i.e. the caller / importer.
        if !edge_keeps(source_kind, source_file_path, &rel_str) {
            continue;
        }

        let entry = serde_json::json!({
            "name": source_node.name.resolve(&graph.string_pool),
            "kind": source_kind,
            "filePath": source_file_path,
            "reason": edge.reason.resolve(&graph.string_pool),
            "confidence": edge.confidence.to_native(),
        });
        incoming.entry(rel_str).or_default().push(entry);
    }

    // Blind spots: only from the same file.
    let blind_spots: Vec<serde_json::Value> = graph
        .blind_spots
        .iter()
        .filter(|bs| bs.file_path.resolve(&graph.string_pool) == file_path_str)
        .map(|bs| {
            serde_json::json!({
                "kind": bs.kind.resolve(&graph.string_pool),
                "line": bs.start_row.to_native(),
                "hint": bs.hint.resolve(&graph.string_pool),
            })
        })
        .collect();

    // 1-hop upstream impact: direct callers/importers.
    let upstream_1hop = bfs_upstream_1hop(graph, node_idx);

    serde_json::json!({
        "symbol": {
            "name": node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&node.kind),
            "filePath": file_path_str,
            "startLine": node.span.0.to_native(),
            "endLine": node.span.2.to_native(),
        },
        "incoming": incoming,
        "outgoing": outgoing,
        "blind_spots": blind_spots,
        "impact_upstream_1hop": upstream_1hop,
    })
}

/// Collect direct callers/importers of `node_idx` (depth=1 upstream).
/// Returns a compact list of `{name, kind, file}` records.
fn bfs_upstream_1hop(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> Vec<serde_json::Value> {
    let mut visited = HashSet::new();
    visited.insert(node_idx);

    let in_start = graph.in_offsets[node_idx].to_native() as usize;
    let in_end = graph.in_offsets[node_idx + 1].to_native() as usize;

    let mut queue = VecDeque::new();
    for i in in_start..in_end {
        let edge_idx = graph.in_edge_idx[i].to_native() as usize;
        let edge = &graph.edges[edge_idx];
        let src_idx = edge.source.to_native() as usize;
        if visited.insert(src_idx) {
            queue.push_back(src_idx);
        }
    }

    let mut results = Vec::new();
    while let Some(idx) = queue.pop_front() {
        let n = &graph.nodes[idx];
        let file = &graph.files[n.file_idx.to_native() as usize];
        results.push(serde_json::json!({
            "name": n.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&n.kind),
            "file": file.path.resolve(&graph.string_pool),
        }));
    }

    results
}

pub fn run(args: InspectArgs, engine: &Engine, graph_path: &Path) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    // Freshness warning: emit to stderr when index is stale.
    let worktree_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Ok(EnsureResult::Stale { age_seconds }) = ensure_index(graph_path, &worktree_root) {
        let age = if age_seconds > 3600 {
            format!("{}h", age_seconds / 3600)
        } else if age_seconds > 60 {
            format!("{}m", age_seconds / 60)
        } else {
            format!("{}s", age_seconds)
        };
        eprintln!("{}", crate::hint::stale_warning("current", &age));
    }

    let name_query = args.name.as_deref().filter(|s| !s.is_empty());

    if name_query.is_none() {
        return Err(GnxError::InvalidArgument("--name is required".to_string()));
    }

    let name = name_query.unwrap();
    let matching_nodes: Vec<(usize, _)> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.name.resolve(&graph.string_pool) == name)
        .collect();

    if matching_nodes.is_empty() {
        let result = serde_json::json!({
            "status": "error",
            "message": format!("Symbol '{}' not found.", name)
        });
        return emit(&result, format);
    }

    // Pre-parse filters once.
    let kind_filter = parse_csv_lower(args.kind.as_deref());
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    let file_substr = args.file_path.as_deref().filter(|s| !s.is_empty());

    if matching_nodes.len() == 1 {
        let (node_idx, _) = matching_nodes[0];
        let block = build_inspect_block(
            graph,
            node_idx,
            &kind_filter,
            &rel_filter,
            file_substr,
            args.include_tests,
        );
        let result = serde_json::json!({
            "status": "found",
            "symbol": block["symbol"],
            "incoming": block["incoming"],
            "outgoing": block["outgoing"],
            "processes": [],
            "blind_spots": block["blind_spots"],
            "impact_upstream_1hop": block["impact_upstream_1hop"],
        });
        return emit(&result, format);
    }

    // Ambiguous: return ALL matches as full inspect blocks (not a candidates list).
    let blocks: Vec<serde_json::Value> = matching_nodes
        .iter()
        .map(|(node_idx, _)| {
            build_inspect_block(
                graph,
                *node_idx,
                &kind_filter,
                &rel_filter,
                file_substr,
                args.include_tests,
            )
        })
        .collect();

    let result = serde_json::json!({
        "status": "ambiguous",
        "message": format!(
            "Found {} symbols matching '{}'. Use --file_path or --kind to disambiguate.",
            blocks.len(),
            name
        ),
        "matches": blocks,
    });
    emit(&result, format)
}
