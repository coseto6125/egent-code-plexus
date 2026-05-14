use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::{Args, ValueEnum};
use graph_nexus_core::algorithms::process_trace::is_test_path;
use graph_nexus_core::{GnxError, HIGH_TRUST_CONFIDENCE};
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

    /// Skip edges with confidence < 0.8 (e.g. framework-aware refs like
    /// FastAPI `Depends()`, Axum/Express route handler bindings). Default
    /// off — all edges traversed.
    #[arg(long, default_value_t = false)]
    pub high_trust_only: bool,

    /// Minimum confidence threshold (0.0 to 1.0) for edges to traverse. Overrides --high-trust-only if provided.
    #[arg(long)]
    pub min_confidence: Option<f32>,

    /// If false (default), nodes located in typical test files/directories are omitted from the output.
    #[arg(long, default_value_t = false)]
    pub include_tests: bool,
}

pub fn run(args: ImpactArgs, engine: &Engine) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

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
            let result = serde_json::json!({
                "error": format!("Symbol UID '{}' not found", args.target)
            });
            return emit(&result, format);
        }
    };

    // BFS Traversal
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut results = Vec::new();

    // queue stores (node_idx, current_depth)
    queue.push_back((start_idx, 0));
    visited.insert(start_idx);

    let min_conf = args.min_confidence.unwrap_or(if args.high_trust_only { HIGH_TRUST_CONFIDENCE } else { 0.0 });
    let mut test_path_cache = std::collections::HashMap::new();

    while let Some((curr_idx, curr_depth)) = queue.pop_front() {
        let curr_node = &graph.nodes[curr_idx];
        let file_idx = curr_node.file_idx.to_native() as usize;

        if !args.include_tests {
            let is_test = *test_path_cache.entry(file_idx).or_insert_with(|| {
                let file_node = &graph.files[file_idx];
                let file_path = file_node.path.resolve(&graph.string_pool);
                is_test_path(file_path)
            });
            if is_test {
                continue;
            }
        }

        let file_node = &graph.files[file_idx];
        let file_path = file_node.path.resolve(&graph.string_pool);

        results.push(serde_json::json!({
            "uid": curr_node.uid.resolve(&graph.string_pool),
            "name": curr_node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&curr_node.kind),
            "filePath": file_path,
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
                    if edge.confidence.to_native() < min_conf {
                        continue;
                    }
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
                    if edge.confidence.to_native() < min_conf {
                        continue;
                    }
                    let next_idx = edge.target.to_native() as usize;
                    if !visited.contains(&next_idx) {
                        visited.insert(next_idx);
                        queue.push_back((next_idx, curr_depth + 1));
                    }
                }
            }
        }
    }

    let result = serde_json::json!({
        "status": "success",
        "target": args.target,
        "direction": match args.direction {
            Direction::Upstream => "upstream",
            Direction::Downstream => "downstream",
        },
        "impact": results,
    });

    emit(&result, format)
}
