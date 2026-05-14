use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;

#[derive(Args, Debug)]
pub struct ClusterArgs {
    #[arg(long)]
    pub repo: Option<String>,

    /// Community/Cluster ID to list
    #[arg(long)]
    pub id: Option<u16>,

    /// Community/Cluster Anchor Name (optional way to lookup ID)
    #[arg(long)]
    pub name: Option<String>,

    /// Output format
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,
}

pub fn run(args: ClusterArgs, engine: &Engine) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    let mut target_id = args.id;

    if target_id.is_none() {
        if let Some(ref name) = args.name {
            for node in graph.nodes.iter() {
                if node.name.resolve(&graph.string_pool) == name {
                    target_id = Some(node.community_id.to_native());
                    break;
                }
            }
            if target_id.is_none() {
                let result = serde_json::json!({
                    "status": "error",
                    "message": format!("No symbol found with name '{}' to act as cluster anchor.", name)
                });
                return emit(&result, format);
            }
        } else {
            let result = serde_json::json!({
                "status": "error",
                "message": "Must specify either --id or --name."
            });
            return emit(&result, format);
        }
    }

    let cid = target_id.unwrap();
    let mut members = Vec::new();

    for node in graph.nodes.iter() {
        if node.community_id.to_native() == cid {
            let file_node = &graph.files[node.file_idx.to_native() as usize];
            members.push(serde_json::json!({
                "uid": node.uid.resolve(&graph.string_pool),
                "name": node.name.resolve(&graph.string_pool),
                "kind": kind_to_str(&node.kind),
                "filePath": file_node.path.resolve(&graph.string_pool),
                "line": node.span.0.to_native(),
            }));
        }
    }

    let result = serde_json::json!({
        "status": "success",
        "cluster_id": cid,
        "member_count": members.len(),
        "members": members,
    });

    emit(&result, format)
}
