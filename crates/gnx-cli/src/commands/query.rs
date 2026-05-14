use crate::engine::Engine;
use clap::Args;
use gnx_core::graph::ArchivedNodeKind;
use rayon::prelude::*;

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// Query string to match against symbol names
    #[arg(long)]
    pub query: String,

    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long, default_value = "text")]
    pub format: Option<String>,
}

pub fn run(args: QueryArgs, engine: &Engine) -> Result<(), String> {
    let graph = engine.graph().map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    let query_lower = args.query.to_lowercase();
    let mut used_semantic = false;

    if let Some(vectors) = graph.embeddings.as_ref() {
        if let Ok(embedder) = gnx_analyzer::embeddings::Embedder::new() {
            if let Ok(mut query_vectors) = embedder.embed(vec![args.query.clone()]) {
                if let Some(query_vec) = query_vectors.pop() {
                    used_semantic = true;

                    let q_norm = query_vec.iter().map(|v| v * v).sum::<f32>().sqrt();

                    let mut scored_nodes: Vec<_> = graph.nodes
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
        for node in graph.nodes.iter() {
            let name = node.name.resolve(&graph.string_pool);
            if name.to_lowercase().contains(&query_lower) {
                let file_node = &graph.files[node.file_idx.to_native() as usize];
                
                results.push(serde_json::json!(format!(
                    "[{}] {}:{} ({})",
                    kind_to_str(&node.kind),
                    file_node.path.resolve(&graph.string_pool),
                    node.span.0.to_native() + 1,
                    name
                )));
            }
        }
    }

    let json = serde_json::json!({
        "status": "success",
        "results": results,
    });

    if args.format.as_deref() == Some("toon") {
        let bytes = serde_json::to_vec(&json).map_err(|e| e.to_string())?;
        let output = _etoon::toon::encode(&bytes).map_err(|e| e.to_string())?;
        println!("{}", output);
    } else if args.format.as_deref() == Some("json") {
        let s = serde_json::to_string(&json).map_err(|e| e.to_string())?;
        println!("{}", s);
    } else {
        for r in &results {
            if let Some(s) = r.as_str() {
                println!("{}", s);
            }
        }
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
        ArchivedNodeKind::Variable => "Variable",
        ArchivedNodeKind::Const => "Const",
        ArchivedNodeKind::Import => "Import",
        ArchivedNodeKind::Route => "Route",
    }
}
