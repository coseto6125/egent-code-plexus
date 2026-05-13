use clap::Args;
use crate::engine::Engine;
use gnx_core::graph::ArchivedNodeKind;

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// Query string to match against symbol names
    #[arg(long)]
    pub query: String,
}

pub fn run(args: QueryArgs, engine: &Engine) -> Result<(), String> {
    let graph = engine.graph().map_err(|e| e.to_string())?;
    
    let mut results = Vec::new();
    let query_lower = args.query.to_lowercase();

    for node in graph.nodes.iter() {
        let name = node.name.resolve(&graph.string_pool);
        if name.to_lowercase().contains(&query_lower) {
            let file_node = &graph.files[node.file_idx.to_native() as usize];
            results.push(serde_json::json!({
                "uid": node.uid.resolve(&graph.string_pool),
                "name": name,
                "kind": kind_to_str(&node.kind),
                "filePath": file_node.path.resolve(&graph.string_pool),
                "line": node.span.0.to_native(),
                "score": 1.0,
            }));
        }
    }

    let json = serde_json::json!({
        "status": "success",
        "results": results,
    });

    match serde_json::to_string(&json) {
        Ok(s) => println!("{}", s),
        Err(e) => return Err(e.to_string()),
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
