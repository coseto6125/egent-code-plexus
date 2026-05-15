use crate::commands::format::{kind_to_str, rel_to_str};
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::algorithms::process_trace::is_test_path;
use graph_nexus_core::GnxError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Query the call-graph context of a single symbol — returns its incoming and
/// outgoing edges, file metadata, and any blind spots detected in the same
/// source file.
#[derive(Args, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ContextArgs {
    /// Name of the symbol to query. Mutually exclusive with --uid.
    #[arg(long)]
    pub name: Option<String>,

    /// UID of the symbol to query (format `Kind:filePath:name`). When set,
    /// disambiguates a collision that `--name` alone cannot resolve.
    #[arg(long)]
    pub uid: Option<String>,

    /// Repository path
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format (e.g. `toon`, `json`). Defaults to `toon`.
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

pub fn run_inner(
    args: ContextArgs,
    engine: &dyn graph_nexus_mcp::registry::EngineRef,
) -> Result<serde_json::Value, GnxError> {
    let engine = crate::engine::cast_engine(engine)?;
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    // Resolve the symbol. --uid wins over --name when both are supplied so
    // CLAUDE.md's "Use UID from candidates" disambiguation step always
    // selects exactly that node.
    let uid_query = args.uid.as_deref().filter(|s| !s.is_empty());
    let name_query = args.name.as_deref().filter(|s| !s.is_empty());

    if uid_query.is_none() && name_query.is_none() {
        return Err(GnxError::InvalidArgument(
            "either --name or --uid is required".to_string(),
        ));
    }

    let mut matching_nodes = Vec::new();
    if let Some(uid) = uid_query {
        for (i, node) in graph.nodes.iter().enumerate() {
            if node.uid.resolve(&graph.string_pool) == uid {
                matching_nodes.push((i, node));
                break;
            }
        }
    } else if let Some(name) = name_query {
        for (i, node) in graph.nodes.iter().enumerate() {
            if node.name.resolve(&graph.string_pool) == name {
                matching_nodes.push((i, node));
            }
        }
    }

    if matching_nodes.is_empty() {
        let needle = uid_query.unwrap_or_else(|| name_query.unwrap_or(""));
        return Ok(serde_json::json!({
            "status": "error",
            "message": format!("Symbol '{}' not found.", needle)
        }));
    }

    if matching_nodes.len() > 1 {
        let mut candidates = Vec::new();
        let display_name = name_query.unwrap_or("");
        for (_, node) in matching_nodes {
            let file_node = &graph.files[node.file_idx.to_native() as usize];
            candidates.push(serde_json::json!({
                "uid": node.uid.resolve(&graph.string_pool),
                "name": node.name.resolve(&graph.string_pool),
                "kind": kind_to_str(&node.kind),
                "filePath": file_node.path.resolve(&graph.string_pool),
                "line": node.span.0.to_native(),
                "score": 1.0,
            }));
        }
        return Ok(serde_json::json!({
            "status": "ambiguous",
            "message": format!("Found {} symbols matching '{}'. Use uid, file_path, or kind to disambiguate.", candidates.len(), display_name),
            "candidates": candidates
        }));
    }

    let (node_idx, node) = matching_nodes[0];
    let file_node = &graph.files[node.file_idx.to_native() as usize];
    let file_path_str = file_node.path.resolve(&graph.string_pool);

    // Pre-parse filters once so the edge loop only does cheap comparisons.
    let kind_filter = parse_csv_lower(args.kind.as_deref());
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    let file_substr = args.file_path.as_deref().filter(|s| !s.is_empty());

    // Returns true when the edge entry should be kept.
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
        if !args.include_tests && is_test_path(target_file_path) {
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
            "uid": target_node.uid.resolve(&graph.string_pool),
            "name": target_node.name.resolve(&graph.string_pool),
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
        // i.e. the caller / importer. CLAUDE.md's --kind / --file_path /
        // --include_tests are about "show me only edges whose other side
        // matches", regardless of direction.
        if !edge_keeps(source_kind, source_file_path, &rel_str) {
            continue;
        }

        let entry = serde_json::json!({
            "uid": source_node.uid.resolve(&graph.string_pool),
            "name": source_node.name.resolve(&graph.string_pool),
            "filePath": source_file_path,
            "reason": edge.reason.resolve(&graph.string_pool),
            "confidence": edge.confidence.to_native(),
        });
        incoming.entry(rel_str).or_default().push(entry);
    }

    // Blind spots are file-level metadata; surface only sites in the same
    // file as the queried symbol so the LLM sees unresolvable patterns it
    // should manually inspect when reading this symbol's context.
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

    Ok(serde_json::json!({
        "status": "found",
        "symbol": {
            "uid": node.uid.resolve(&graph.string_pool),
            "name": node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&node.kind),
            "filePath": file_path_str,
            "startLine": node.span.0.to_native(),
            "endLine": node.span.2.to_native(),
        },
        "incoming": incoming,
        "outgoing": outgoing,
        "processes": [],
        "blind_spots": blind_spots,
    }))
}

pub fn run(args: ContextArgs, engine: &Engine) -> Result<(), GnxError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let value = run_inner(args, engine)?;
    emit(&value, format)
}

#[cfg(test)]
mod inner_tests {
    use super::*;

    #[test]
    fn run_inner_returns_structured_value_not_unit() {
        // Compile-only signature check. Behaviour is verified by the
        // command's existing integration tests when called via run().
        fn _accepts(
            _f: fn(
                ContextArgs,
                &dyn graph_nexus_mcp::registry::EngineRef,
            ) -> Result<serde_json::Value, graph_nexus_core::GnxError>,
        ) {
        }
        _accepts(run_inner);
    }
}

graph_nexus_mcp::gnx_register_mcp_tool!(ContextArgs, run_inner);
