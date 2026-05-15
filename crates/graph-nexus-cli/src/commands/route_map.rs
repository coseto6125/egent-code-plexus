use clap::Args;
use graph_nexus_core::graph::ArchivedNodeKind;
use graph_nexus_core::GnxError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// List every Route node in the graph — maps HTTP endpoints to their
/// source locations for downstream handler and consumer lookups.
#[derive(Args, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RouteMapArgs {
    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,
}

pub fn run_inner(
    _args: RouteMapArgs,
    engine: &dyn graph_nexus_mcp::registry::EngineRef,
) -> Result<serde_json::Value, GnxError> {
    let engine = crate::engine::cast_engine(engine)?;
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    let mut results = Vec::new();

    for node in graph.nodes.iter() {
        if matches!(&node.kind, ArchivedNodeKind::Route) {
            let name = node.name.resolve(&graph.string_pool);
            let file_node = &graph.files[node.file_idx.to_native() as usize];
            results.push(serde_json::json!({
                "uid": node.uid.resolve(&graph.string_pool),
                "name": name,
                "kind": "Route",
                "filePath": file_node.path.resolve(&graph.string_pool),
                "line": node.span.0.to_native(),
            }));
        }
    }

    let result = serde_json::json!({
        "status": "success",
        "results": results,
    });

    Ok(result)
}

pub fn run(
    args: RouteMapArgs,
    engine: &crate::engine::Engine,
) -> Result<(), graph_nexus_core::GnxError> {
    let format = crate::output::OutputFormat::parse(args.format.as_deref());
    let value = run_inner(args, engine)?;
    crate::output::emit(&value, format)
}

#[cfg(test)]
mod inner_tests {
    use super::*;
    #[test]
    fn run_inner_returns_structured_value_not_unit() {
        fn _accepts(
            _f: fn(
                RouteMapArgs,
                &dyn graph_nexus_mcp::registry::EngineRef,
            ) -> Result<serde_json::Value, graph_nexus_core::GnxError>,
        ) {
        }
        _accepts(run_inner);
    }
}

graph_nexus_mcp::gnx_register_mcp_tool!(RouteMapArgs, run_inner);
