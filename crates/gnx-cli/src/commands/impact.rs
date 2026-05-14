use crate::engine::Engine;
use clap::{Args, ValueEnum};
use gnx_core::graph::ArchivedNodeKind;
use std::collections::{HashSet, VecDeque};

#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum Direction {
    Upstream,
    Downstream,
}

#[derive(Args, Debug)]
pub struct ImpactArgs {
    /// Target symbol UID
    #[arg(long)]
    pub target: String,

    /// Direction of traversal
    #[arg(long, value_enum)]
    pub direction: Direction,

    /// Maximum depth of traversal
    #[arg(long, default_value = "5")]
    pub depth: usize,

    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,
}

pub fn run(args: ImpactArgs, engine: &Engine) -> Result<(), String> {
    let graph = engine.graph().map_err(|e| e.to_string())?;

    // Find the target node index by UID
    let mut start_idx = None;
    for (i, node) in graph.nodes.iter().enumerate() {
        if node.uid.resolve(&graph.string_pool) == args.target {
            start_idx = Some(i);
            break;
        }
    }

    let start_idx = match start_idx {
        Some(idx) => idx,
        None => {
            let json = serde_json::json!({
                "error": format!("Symbol UID '{}' not found", args.target)
            });
            println!("{}", json);
            return Ok(());
        }
    };

    // BFS Traversal
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut results = Vec::new();

    // queue stores (node_idx, current_depth)
    queue.push_back((start_idx, 0));
    visited.insert(start_idx);

    while let Some((curr_idx, curr_depth)) = queue.pop_front() {
        let curr_node = &graph.nodes[curr_idx];
        let file_node = &graph.files[curr_node.file_idx.to_native() as usize];

        results.push(serde_json::json!({
            "uid": curr_node.uid.resolve(&graph.string_pool),
            "name": curr_node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&curr_node.kind),
            "filePath": file_node.path.resolve(&graph.string_pool),
            "line": curr_node.span.0.to_native(),
            "depth": curr_depth,
        }));

        if curr_depth >= args.depth {
            continue;
        }

        match args.direction {
            Direction::Upstream => {
                let in_start = graph.in_offsets[curr_idx].to_native() as usize;
                let in_end = graph.in_offsets[curr_idx + 1].to_native() as usize;
                for i in in_start..in_end {
                    let edge_idx = graph.in_edge_idx[i].to_native() as usize;
                    let edge = &graph.edges[edge_idx];
                    let next_idx = edge.source.to_native() as usize;
                    if !visited.contains(&next_idx) {
                        visited.insert(next_idx);
                        queue.push_back((next_idx, curr_depth + 1));
                    }
                }
            }
            Direction::Downstream => {
                let out_start = graph.out_offsets[curr_idx].to_native() as usize;
                let out_end = graph.out_offsets[curr_idx + 1].to_native() as usize;
                for i in out_start..out_end {
                    let edge = &graph.edges[i];
                    let next_idx = edge.target.to_native() as usize;
                    if !visited.contains(&next_idx) {
                        visited.insert(next_idx);
                        queue.push_back((next_idx, curr_depth + 1));
                    }
                }
            }
        }
    }

    let json = serde_json::json!({
        "status": "success",
        "target": args.target,
        "direction": match args.direction {
            Direction::Upstream => "upstream",
            Direction::Downstream => "downstream",
        },
        "impact": results,
    });

    if args.format.as_deref() == Some("toon") {
        let bytes = serde_json::to_vec(&json).map_err(|e| e.to_string())?;
        let output = _etoon::toon::encode(&bytes).map_err(|e| e.to_string())?;
        println!("{}", output);
    } else {
        let s = serde_json::to_string(&json).map_err(|e| e.to_string())?;
        println!("{}", s);
    }

    Ok(())
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
