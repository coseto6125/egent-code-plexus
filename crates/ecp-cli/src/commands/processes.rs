//! `ecp processes` — execution-flow inspection.
//!
//! Without subcommand: list detected Process nodes (cross-vs-intra
//! community, step count, entry file). With `trace <pattern>`: dump the
//! full Function/Method step sequence for matching Processes.
//!
//! Process nodes are emitted by `pass4_processes` in
//! `crates/ecp-analyzer/src/resolution/builder.rs:1432`, which runs
//! `detect_processes` over the Calls graph (BFS forward with confidence /
//! depth / branching bounds) and stores trace step indices in the CSR
//! `traces_offsets` + `traces_data` pair on `ZeroCopyGraph`.

use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::{Args, Subcommand};
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::EcpError;
use std::collections::HashSet;

#[derive(Args, Debug, Clone)]
pub struct ProcessesArgs {
    #[command(subcommand)]
    pub command: Option<ProcessesCommands>,

    /// Repository selector.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format (toon / json / text).
    #[arg(long)]
    pub format: Option<String>,

    /// Filter the list view to a single community ID.
    #[arg(long)]
    pub community: Option<u32>,

    /// Cap on listed processes (default 50).
    #[arg(long, default_value = "50")]
    pub limit: usize,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ProcessesCommands {
    /// Show the full Function/Method step sequence for Process(es) whose
    /// label substring-matches `<pattern>`. Labels are `"Entry → Terminal"`.
    Trace(TraceArgs),
}

#[derive(Args, Debug, Clone)]
pub struct TraceArgs {
    /// Substring match (case-insensitive) against Process labels.
    pub pattern: String,

    /// Repository selector.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format (toon / json / text).
    #[arg(long)]
    pub format: Option<String>,

    /// Cap on matched Processes shown (default 5).
    #[arg(long, default_value = "5")]
    pub limit: usize,
}

pub fn run(args: ProcessesArgs, engine: &Engine) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    match args.command.clone() {
        None => list_processes(graph, args),
        Some(ProcessesCommands::Trace(t)) => trace_processes(graph, t),
    }
}

fn list_processes(graph: &ArchivedZeroCopyGraph, args: ProcessesArgs) -> Result<(), EcpError> {
    let fmt = OutputFormat::parse(args.format.as_deref());
    let process_start = graph.process_start.to_native();
    let total_nodes = graph.nodes.len() as u32;
    let total_processes = total_nodes.saturating_sub(process_start);

    let wanted_community = args.community.map(|c| c as u16);
    let mut results = Vec::new();

    for idx in process_start..total_nodes {
        let k = (idx - process_start) as usize;
        let node = &graph.nodes[idx as usize];
        let comm = node.community_id.to_native();
        if let Some(want) = wanted_community {
            if comm != want {
                continue;
            }
        }

        let step_start = graph.traces_offsets[k].to_native() as usize;
        let step_end = graph.traces_offsets[k + 1].to_native() as usize;
        let step_count = step_end - step_start;

        let mut comms_seen: HashSet<u16> = HashSet::new();
        for s in step_start..step_end {
            let member_idx = graph.traces_data[s].to_native() as usize;
            let mc = graph.nodes[member_idx].community_id.to_native();
            if mc != 0 {
                comms_seen.insert(mc);
            }
        }
        let process_type = if comms_seen.len() > 1 {
            "cross_community"
        } else {
            "intra_community"
        };

        let label = node.name.resolve(&graph.string_pool);
        let file_path = if node.has_owning_file() {
            graph.files[node.file_idx.to_native() as usize]
                .path
                .resolve(&graph.string_pool)
                .to_string()
        } else {
            String::new()
        };
        results.push(serde_json::json!({
            "uid": node.uid.to_native().to_string(),
            "label": label,
            "community": comm,
            "process_type": process_type,
            "step_count": step_count,
            "filePath": file_path,
            "line": node.span.0.to_native(),
        }));
        if results.len() >= args.limit {
            break;
        }
    }

    if total_processes == 0 {
        eprintln!(
            "No Process nodes emitted.\n\
             → Either the graph is too small (need ≥3-step Calls chains \
             at confidence ≥0.5) or detect_processes thresholds filtered \
             everything out. See `crates/ecp-core/src/algorithms/process_trace.rs`."
        );
    }

    let payload = serde_json::json!({
        "status": "success",
        "total": total_processes,
        "shown": results.len(),
        "community_filter": args.community,
        "results": results,
    });
    emit(&payload, fmt)
}

fn trace_processes(graph: &ArchivedZeroCopyGraph, args: TraceArgs) -> Result<(), EcpError> {
    let fmt = OutputFormat::parse(args.format.as_deref());
    let pattern_lower = args.pattern.to_lowercase();
    let process_start = graph.process_start.to_native();
    let total_nodes = graph.nodes.len() as u32;

    let mut matches = Vec::new();
    for idx in process_start..total_nodes {
        let k = (idx - process_start) as usize;
        let node = &graph.nodes[idx as usize];
        let label = node.name.resolve(&graph.string_pool);
        if !label.to_lowercase().contains(&pattern_lower) {
            continue;
        }

        let step_start = graph.traces_offsets[k].to_native() as usize;
        let step_end = graph.traces_offsets[k + 1].to_native() as usize;

        let mut steps = Vec::with_capacity(step_end - step_start);
        for (i, s) in (step_start..step_end).enumerate() {
            let member_idx = graph.traces_data[s].to_native() as usize;
            let member = &graph.nodes[member_idx];
            let file_path = if member.has_owning_file() {
                graph.files[member.file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.string_pool)
                    .to_string()
            } else {
                String::new()
            };
            steps.push(serde_json::json!({
                "step": i + 1,
                "name": member.name.resolve(&graph.string_pool),
                "kind": kind_to_str(&member.kind),
                "filePath": file_path,
                "line": member.span.0.to_native(),
                "community": member.community_id.to_native(),
            }));
        }
        matches.push(serde_json::json!({
            "label": label,
            "uid": node.uid.to_native().to_string(),
            "community": node.community_id.to_native(),
            "steps": steps,
        }));
        if matches.len() >= args.limit {
            break;
        }
    }

    if matches.is_empty() {
        let payload = serde_json::json!({
            "status": "not_found",
            "pattern": args.pattern,
            "candidates": [],
        });
        return emit(&payload, fmt);
    }

    let payload = serde_json::json!({
        "status": "success",
        "pattern": args.pattern,
        "matched": matches.len(),
        "results": matches,
    });
    emit(&payload, fmt)
}
