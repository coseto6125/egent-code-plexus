use clap::Args;
use graph_nexus_core::graph::{NodeKind, RelType};
use crate::engine::Engine;
use regex::Regex;
use std::sync::LazyLock;

static CYPHER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)MATCH\s+\((?P<src_var>\w+):(?P<src_kind>\w+)\)\s*<*-\[\s*(?P<rel_var>\w*)?(?::(?P<rel_type>\w+))?\s*(?P<star>\*)?\s*(?P<min_len>\d*)?\s*(?:\.\.\s*(?P<max_len>\d*)?)?\s*\]->*\s*\((?P<tgt_var>\w+):(?P<tgt_kind>\w+)\)\s*(?:WHERE\s+(?P<where_var>\w+)\.(?P<where_prop>\w+)\s*=\s*'(?P<where_val>[^']+)')?\s*RETURN\s+(?P<ret>[\w\.,\s]+)"
    ).expect("Failed to compile Cypher regex")
});

#[derive(Args, Debug, Clone)]
pub struct CypherArgs {
    /// The Cypher query string
    pub query: String,

    #[arg(long)]
    pub repo: Option<String>,

    #[arg(long, default_value = "json")]
    pub format: String,
}

pub fn run(args: CypherArgs, engine: &Engine) -> Result<(), graph_nexus_core::GnxError> {
    let graph = engine.graph().map_err(|e| graph_nexus_core::GnxError::Rkyv(e.to_string()))?;

    let caps = match CYPHER_REGEX.captures(&args.query) {
        Some(c) => c,
        None => {
            return Err(graph_nexus_core::GnxError::InvalidArgument(
                "Query not supported. Minimal Cypher supports: MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE a.name='Val'] RETURN a,b".to_string(),
            ));
        }
    };

    let src_kind: Option<NodeKind> = caps.name("src_kind").and_then(|m| m.as_str().parse().ok());
    let tgt_kind: Option<NodeKind> = caps.name("tgt_kind").and_then(|m| m.as_str().parse().ok());
    let rel_type: Option<RelType> = caps.name("rel_type").and_then(|m| m.as_str().parse().ok());

    let is_variable = caps.name("star").is_some();
    let min_len: usize = caps.name("min_len")
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(if is_variable { 1 } else { 1 });
    let max_len: usize = caps.name("max_len")
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(if is_variable { usize::MAX } else { 1 });

    let where_clause = caps.name("where_var").map(|var| {
        (
            var.as_str(),
            caps.name("where_prop").unwrap().as_str(),
            caps.name("where_val").unwrap().as_str(),
        )
    });

    let mut results = Vec::new();

    for (src_idx, node) in graph.nodes.iter().enumerate() {
        if src_kind.is_some_and(|k| node.kind != k) {
            continue;
        }

        let s = graph.out_offsets[src_idx].to_native() as usize;
        let e = graph.out_offsets[src_idx + 1].to_native() as usize;
        let edges_slice = &graph.edges[s..e];

        if edges_slice.is_empty() {
            continue;
        }

        if let Some((_, prop, val)) = where_clause {
            if prop == "name" {
                let name = node.name.resolve(&graph.string_pool);
                if name != val {
                    continue;
                }
            }
        }

        let name = node.name.resolve(&graph.string_pool);

        if !is_variable {
            for edge in edges_slice {
                if rel_type.is_some_and(|req_rel| edge.rel_type != req_rel) {
                    continue;
                }

                let tgt_idx = edge.target.to_native() as usize;
                let tgt_node = &graph.nodes[tgt_idx];

                if tgt_kind.is_some_and(|k| tgt_node.kind != k) {
                    continue;
                }

                let tgt_name = tgt_node.name.resolve(&graph.string_pool);
                let file_idx = tgt_node.file_idx.to_native() as usize;
                
                let tgt_path = if file_idx < graph.files.len() {
                    graph.files[file_idx].path.resolve(&graph.string_pool)
                } else {
                    ""
                };

                results.push(serde_json::json!({
                    "source": {
                        "name": name.to_string(),
                    },
                    "target": {
                        "name": tgt_name.to_string(),
                        "filePath": tgt_path.to_string(),
                        "kind": format!("{:?}", tgt_node.kind),
                    }
                }));
            }
        } else {
            let callees = graph_nexus_core::graph_query::callees_of(graph, src_idx as u32, max_len);
            
            for (tgt_idx, depth) in callees {
                if depth < min_len || depth > max_len {
                    continue;
                }

                let tgt_node = &graph.nodes[tgt_idx as usize];

                if tgt_kind.is_some_and(|k| tgt_node.kind != k) {
                    continue;
                }

                let tgt_name = tgt_node.name.resolve(&graph.string_pool);
                let file_idx = tgt_node.file_idx.to_native() as usize;
                
                let tgt_path = if file_idx < graph.files.len() {
                    graph.files[file_idx].path.resolve(&graph.string_pool)
                } else {
                    ""
                };

                results.push(serde_json::json!({
                    "source": {
                        "name": name.to_string(),
                    },
                    "target": {
                        "name": tgt_name.to_string(),
                        "filePath": tgt_path.to_string(),
                        "kind": format!("{:?}", tgt_node.kind),
                    },
                    "depth": depth
                }));
            }
        }
    }

    let output = serde_json::json!({
        "results": results
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());

    Ok(())
}