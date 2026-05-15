use crate::commands::format::kind_to_str;
use clap::Args;
use graph_nexus_core::GnxError;
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Search for symbols by semantic similarity or BM25 full-text and return
/// ranked results across the indexed graph.
#[derive(Args, Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueryArgs {
    /// Query string to match against symbol names
    #[arg(long)]
    pub query: String,

    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long, default_value = "text")]
    pub format: Option<String>,
}

pub fn run_inner(
    args: QueryArgs,
    engine: &dyn graph_nexus_mcp::registry::EngineRef,
) -> Result<serde_json::Value, GnxError> {
    let engine = crate::engine::cast_engine(engine)?;
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    let mut results = Vec::new();
    let mut used_semantic = false;

    if let Some(vectors) = graph.embeddings.as_ref() {
        if let Ok(embedder) = graph_nexus_analyzer::embeddings::Embedder::new() {
            if let Ok(mut query_vectors) = embedder.embed(vec![args.query.clone()]) {
                if let Some(query_vec) = query_vectors.pop() {
                    used_semantic = true;

                    let q_norm = query_vec.iter().map(|v| v * v).sum::<f32>().sqrt();

                    let mut scored_nodes: Vec<_> = graph
                        .nodes
                        .par_iter()
                        .enumerate()
                        .filter_map(|(idx, node)| {
                            let node_vec = vectors.get(idx)?;
                            let mut dot_product = 0.0;
                            let mut n_norm_sq = 0.0;
                            for (q_val, n_val) in query_vec.iter().zip(node_vec.iter()) {
                                #[allow(clippy::useless_conversion)]
                                let n: f32 = n_val.into();
                                dot_product += q_val * n;
                                n_norm_sq += n * n;
                            }
                            let n_norm = n_norm_sq.sqrt();
                            let similarity = if q_norm > 0.0 && n_norm > 0.0 {
                                dot_product / (q_norm * n_norm)
                            } else {
                                0.0
                            };
                            Some((similarity, node))
                        })
                        .collect();

                    scored_nodes.par_sort_unstable_by(|a, b| {
                        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
                    });

                    for (similarity, node) in scored_nodes.into_iter().take(20) {
                        let name = node.name.resolve(&graph.string_pool);
                        let file_node = &graph.files[node.file_idx.to_native() as usize];

                        // Output the highly token-optimized string format
                        results.push(serde_json::json!(format!(
                            "[{}] {}:{} ({}) [score:{:.4}]",
                            kind_to_str(&node.kind),
                            file_node.path.resolve(&graph.string_pool),
                            node.span.0.to_native() + 1, // Convert 0-based to 1-based
                            name,
                            similarity
                        )));
                    }
                }
            }
        }
    }

    if !used_semantic {
        let repo_path = std::path::PathBuf::from(args.repo.as_deref().unwrap_or("."));
        let tantivy_results = crate::search::TantivyEngine::search(&repo_path, &args.query);

        for (score, uid) in tantivy_results {
            if let Some(node) = graph
                .nodes
                .iter()
                .find(|n| n.uid.resolve(&graph.string_pool) == uid)
            {
                let name = node.name.resolve(&graph.string_pool);
                let file_node = &graph.files[node.file_idx.to_native() as usize];

                results.push(serde_json::json!(format!(
                    "[{}] {}:{} ({}) [bm25:{:.4}]",
                    kind_to_str(&node.kind),
                    file_node.path.resolve(&graph.string_pool),
                    node.span.0.to_native() + 1,
                    name,
                    score
                )));
            }
        }
    }

    let result = serde_json::json!({
        "status": "success",
        "results": results,
    });

    Ok(result)
}

pub fn run(
    args: QueryArgs,
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
                QueryArgs,
                &dyn graph_nexus_mcp::registry::EngineRef,
            ) -> Result<serde_json::Value, graph_nexus_core::GnxError>,
        ) {
        }
        _accepts(run_inner);
    }
}

graph_nexus_mcp::gnx_register_mcp_tool!(QueryArgs, run_inner);
