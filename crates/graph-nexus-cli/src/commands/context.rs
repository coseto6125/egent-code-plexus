use crate::commands::format::{kind_to_str, rel_to_str};
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;
use std::collections::HashMap;

#[derive(Args, Debug)]
pub struct ContextArgs {
    /// Name of the symbol to query
    #[arg(long)]
    pub name: String,

    /// Repository path
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,
}

pub fn run(args: ContextArgs, engine: &Engine) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    // Find matching nodes
    let mut matching_nodes = Vec::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        if node.name.resolve(&graph.string_pool) == args.name {
            matching_nodes.push((i, node));
        }
    }

    if matching_nodes.is_empty() {
        let result = serde_json::json!({
            "status": "error",
            "message": format!("Symbol '{}' not found.", args.name)
        });
        return emit(&result, format);
    }

    if matching_nodes.len() > 1 {
        let mut candidates = Vec::new();
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
        let result = serde_json::json!({
            "status": "ambiguous",
            "message": format!("Found {} symbols matching '{}'. Use uid, file_path, or kind to disambiguate.", candidates.len(), args.name),
            "candidates": candidates
        });
        return emit(&result, format);
    }

    let (node_idx, node) = matching_nodes[0];
    let file_node = &graph.files[node.file_idx.to_native() as usize];
    let file_path_str = file_node.path.resolve(&graph.string_pool);

    let mut incoming: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut outgoing: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

    // Outgoing edges
    let out_start = graph.out_offsets[node_idx].to_native() as usize;
    let out_end = graph.out_offsets[node_idx + 1].to_native() as usize;
    for i in out_start..out_end {
        let edge = &graph.edges[i];
        let target_node = &graph.nodes[edge.target.to_native() as usize];
        let target_file = &graph.files[target_node.file_idx.to_native() as usize];

        let rel_str = rel_to_str(&edge.rel_type).to_string();
        let entry = serde_json::json!({
            "uid": target_node.uid.resolve(&graph.string_pool),
            "name": target_node.name.resolve(&graph.string_pool),
            "filePath": target_file.path.resolve(&graph.string_pool),
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

        let rel_str = rel_to_str(&edge.rel_type).to_string();
        let entry = serde_json::json!({
            "uid": source_node.uid.resolve(&graph.string_pool),
            "name": source_node.name.resolve(&graph.string_pool),
            "filePath": source_file.path.resolve(&graph.string_pool),
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

    let result = serde_json::json!({
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
    });

    emit(&result, format)
}
