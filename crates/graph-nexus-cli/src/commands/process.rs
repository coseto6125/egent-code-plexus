use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;

#[derive(Args, Debug)]
pub struct ProcessArgs {
    /// Name of the process to query
    #[arg(long)]
    pub name: String,

    /// Repository path
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long, default_value = "json")]
    pub format: Option<String>,
}

pub fn run(args: ProcessArgs, engine: &Engine) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    let process_start = graph.process_start.to_native() as usize;
    let mut matching_nodes = Vec::new();

    // Find matching nodes that are processes
    for (i, node) in graph.nodes.iter().enumerate().skip(process_start) {
        if node.name.resolve(&graph.string_pool) == args.name {
            matching_nodes.push((i, node));
        }
    }

    if matching_nodes.is_empty() {
        let result = serde_json::json!({
            "status": "error",
            "message": format!("Process '{}' not found.", args.name)
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
            }));
        }
        let result = serde_json::json!({
            "status": "ambiguous",
            "message": format!("Found {} processes matching '{}'.", candidates.len(), args.name),
            "candidates": candidates
        });
        return emit(&result, format);
    }

    let (node_idx, node) = matching_nodes[0];
    let file_node = &graph.files[node.file_idx.to_native() as usize];
    let file_path_str = file_node.path.resolve(&graph.string_pool);

    let process_k = node_idx - process_start;
    let trace_start = graph.traces_offsets[process_k].to_native() as usize;
    let trace_end = graph.traces_offsets[process_k + 1].to_native() as usize;

    let mut steps = Vec::new();
    for i in trace_start..trace_end {
        let step_node_idx = graph.traces_data[i].to_native() as usize;
        let step_node = &graph.nodes[step_node_idx];
        let step_file = &graph.files[step_node.file_idx.to_native() as usize];
        
        steps.push(serde_json::json!({
            "uid": step_node.uid.resolve(&graph.string_pool),
            "name": step_node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&step_node.kind),
            "filePath": step_file.path.resolve(&graph.string_pool),
            "line": step_node.span.0.to_native(),
        }));
    }

    let result = serde_json::json!({
        "status": "success",
        "process": {
            "uid": node.uid.resolve(&graph.string_pool),
            "name": node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&node.kind),
            "filePath": file_path_str,
            "startLine": node.span.0.to_native(),
            "endLine": node.span.2.to_native(),
        },
        "trace": steps,
    });

    emit(&result, format)
}
