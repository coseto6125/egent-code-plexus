use crate::engine::Engine;
use crate::repo_selector;
use clap::Args;
use graph_nexus_core::cypher;
use graph_nexus_core::registry::RegistryFile;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct CypherArgs {
    /// The Cypher query string. Accepts the positional form
    /// (`gnx cypher "MATCH ..."`) — the `--query` named form below
    /// stays as an alias for parity with old MCP / wrapper habits.
    #[arg(value_name = "QUERY")]
    pub query_positional: Option<String>,

    /// Named alias for the positional QUERY argument.
    #[arg(long = "query", value_name = "QUERY", conflicts_with = "query_positional")]
    pub query: Option<String>,

    /// Repository to query. Cypher operates on a single graph (single-repo only).
    /// If --repo resolves to multiple repos, an error is returned.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: `json` (default, column-based) or `toon` (LLM-friendly compact).
    #[arg(long, default_value = "json")]
    pub format: String,
}

impl CypherArgs {
    fn resolved_query(&self) -> Result<&str, graph_nexus_core::GnxError> {
        self.query
            .as_deref()
            .or(self.query_positional.as_deref())
            .ok_or_else(|| {
                graph_nexus_core::GnxError::InvalidArgument(
                    "cypher requires a query — pass it positionally (gnx cypher \"MATCH ...\") or via --query".into(),
                )
            })
    }
}

fn resolve_repo_root(repo_arg: Option<&str>) -> PathBuf {
    if let Some(r) = repo_arg {
        return PathBuf::from(r);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn run(args: CypherArgs, engine: &Engine) -> Result<(), graph_nexus_core::GnxError> {
    // Multi-repo gate: cypher is single-repo only (graph identity is per-repo).
    if let Some(repo_sel) = args.repo.as_deref() {
        let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
        let registry =
            RegistryFile::read_or_empty(&home_gnx.join("registry.json")).map_err(|e| {
                graph_nexus_core::GnxError::InvalidArgument(format!("registry read: {e}"))
            })?;
        let selector = repo_selector::parse(repo_sel).map_err(|e| {
            graph_nexus_core::GnxError::InvalidArgument(format!("--repo selector: {e}"))
        })?;
        let cwd = std::env::current_dir().unwrap_or_default();
        let repos = repo_selector::resolve(&selector, &registry, cwd.to_str().unwrap_or("."))
            .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("--repo: {e}")))?;
        if repos.len() > 1 {
            return Err(graph_nexus_core::GnxError::InvalidArgument(format!(
                "cypher is single-repo only (graph identity); --repo resolved to {} repos. Pick one with --repo <name|path>.",
                repos.len()
            )));
        }
    }

    let graph = engine
        .graph()
        .map_err(|e| graph_nexus_core::GnxError::Rkyv(e.to_string()))?;

    let query_str = args.resolved_query()?;
    let query = cypher::parse(query_str)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format_cypher_error(query_str, &e)))?;

    let result = cypher::execute(&query, graph, &resolve_repo_root(args.repo.as_deref()))
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format_cypher_error(query_str, &e)))?;

    match args.format.as_str() {
        "toon" => println!("{}", serialize_toon(&result)),
        _ => println!("{}", serialize_json(&result)),
    }
    Ok(())
}

fn format_cypher_error(query: &str, e: &cypher::CypherError) -> String {
    // Best-effort: print query then `^` indicator. Refined in D4.
    format!("{e}\nquery: {query}")
}

fn serialize_json(_r: &cypher::QueryResult) -> String {
    unimplemented!("D2")
}

fn serialize_toon(_r: &cypher::QueryResult) -> String {
    unimplemented!("D3")
}
