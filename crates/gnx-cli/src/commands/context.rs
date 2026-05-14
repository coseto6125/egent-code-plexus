use crate::engine::Engine;
use clap::Args;
use gnx_core::graph::{ArchivedNodeKind, ArchivedRelType};
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

pub fn run(args: ContextArgs, engine: &Engine) -> Result<(), String> {
    let graph = engine.graph().map_err(|e| e.to_string())?;

    // Find matching nodes
    let mut matching_nodes = Vec::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        if node.name.resolve(&graph.string_pool) == args.name {
            matching_nodes.push((i, node));
        }
    }

    if matching_nodes.is_empty() {
        let json = serde_json::json!({
            "error": format!("Symbol '{}' not found", args.name)
        });
        println!("{}", json);
        return Ok(());
    }

    fn kind_to_str(kind: &ArchivedNodeKind) -> &'static str {
        match kind {
            ArchivedNodeKind::File => "File",
            ArchivedNodeKind::Function => "Function",
            ArchivedNodeKind::Class => "Class",
            ArchivedNodeKind::Method => "Method",
            ArchivedNodeKind::Interface => "Interface",
            ArchivedNodeKind::Constructor => "Constructor",
            ArchivedNodeKind::Property => "Property",
            ArchivedNodeKind::Import => "Import",
        }
    }

    fn rel_to_str(rel: &ArchivedRelType) -> &'static str {
        match rel {
            ArchivedRelType::Defines => "defines",
            ArchivedRelType::Imports => "imports",
            ArchivedRelType::Calls => "calls",
            ArchivedRelType::HasMethod => "has_method",
            ArchivedRelType::HasProperty => "has_property",
            ArchivedRelType::Accesses => "accesses",
        }
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
        let json = serde_json::json!({
            "status": "ambiguous",
            "message": format!("Found {} symbols matching '{}'. Use uid, file_path, or kind to disambiguate.", candidates.len(), args.name),
            "candidates": candidates
        });
        match serde_json::to_string(&json) {
            Ok(s) => println!("{}", s),
            Err(e) => return Err(e.to_string()),
        }
        return Ok(());
    }

    let (node_idx, node) = matching_nodes[0];
    let file_node = &graph.files[node.file_idx.to_native() as usize];

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
        });
        incoming.entry(rel_str).or_default().push(entry);
    }

    let result = serde_json::json!({
        "status": "found",
        "symbol": {
            "uid": node.uid.resolve(&graph.string_pool),
            "name": node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&node.kind),
            "filePath": file_node.path.resolve(&graph.string_pool),
            "startLine": node.span.0.to_native() - 1,
            "endLine": node.span.2.to_native() - 1,
        },
        "incoming": incoming,
        "outgoing": outgoing,
        "processes": []
    });

    if args.format.as_deref() == Some("toon") {
        let bytes = serde_json::to_vec(&result).map_err(|e| e.to_string())?;
        let output = _etoon::toon::encode(&bytes).map_err(|e| e.to_string())?;
        println!("{}", output);
    } else {
        let s = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
        println!("{}", s);
    }
    Ok(())
}
