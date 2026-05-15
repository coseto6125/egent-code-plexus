use crate::engine::Engine;
use crate::repo_selector;
use clap::Args;
use graph_nexus_core::cypher;
use graph_nexus_core::registry::RegistryFile;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct CypherArgs {
    /// The Cypher query string. Supports a read-only subset of openCypher:
    ///
    /// - Multi-hop patterns: (a)-[:Calls]->(b)-[:Calls]->(c)
    /// - Variable-length:    (a)-[:Calls*1..3]->(b)
    /// - Label alternation:  (a:Function|Method)
    /// - WHERE:              =, <>, <, <=, >, >=, AND, OR, NOT, IN, =~, CONTAINS, STARTS WITH, ENDS WITH
    /// - Properties:         a.name, a.kind, a.filePath, r.confidence, r.reason
    /// - Aggregation:        COUNT(*), COUNT(DISTINCT x), SUM/MIN/MAX/AVG, COLLECT
    /// - Pipeline:           WITH ... [WHERE ...], OPTIONAL MATCH, UNION [ALL]
    /// - Output shaping:     RETURN [DISTINCT], ORDER BY, SKIP, LIMIT
    ///
    /// Cypher operates on a single graph; --repo must resolve to one repo.
    #[arg(value_name = "QUERY")]
    pub query_positional: Option<String>,

    /// Named alias for the positional QUERY argument.
    #[arg(
        long = "query",
        value_name = "QUERY",
        conflicts_with = "query_positional"
    )]
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
    let query = cypher::parse(query_str).map_err(|e| {
        graph_nexus_core::GnxError::InvalidArgument(format_cypher_error(query_str, &e))
    })?;

    let result =
        cypher::execute(&query, graph, &resolve_repo_root(args.repo.as_deref())).map_err(|e| {
            graph_nexus_core::GnxError::InvalidArgument(format_cypher_error(query_str, &e))
        })?;

    match args.format.as_str() {
        "toon" => println!("{}", serialize_toon(&result)),
        _ => println!("{}", serialize_json(&result)),
    }
    Ok(())
}

fn format_cypher_error(query: &str, e: &cypher::CypherError) -> String {
    use cypher::CypherError::*;
    let offset = match e {
        Lex { offset, .. } | Parse { offset, .. } => Some(*offset),
        _ => None,
    };
    let mut out = String::new();
    out.push_str(query);
    out.push('\n');
    if let Some(off) = offset {
        // Token-index isn't the same as byte-index; use it as a soft hint
        // and clamp to query length so we never go out of bounds.
        let pad = off.min(query.len());
        out.push_str(&" ".repeat(pad));
        out.push_str("^\n");
    }
    out.push_str(&format!("{e}"));
    out
}

fn serialize_json(r: &cypher::QueryResult) -> String {
    let rows: Vec<serde_json::Value> = r
        .rows
        .iter()
        .map(|row| serde_json::Value::Array(row.iter().map(value_to_json).collect()))
        .collect();
    let out = serde_json::json!({ "columns": r.columns, "rows": rows });
    serde_json::to_string_pretty(&out).unwrap()
}

fn value_to_json(v: &cypher::Value) -> serde_json::Value {
    use cypher::Value::*;
    match v {
        Null => serde_json::Value::Null,
        Bool(b) => serde_json::json!(b),
        Int(i) => serde_json::json!(i),
        Float(f) => serde_json::json!(f),
        Str(s) => serde_json::json!(s),
        List(xs) => serde_json::Value::Array(xs.iter().map(value_to_json).collect()),
        NodeRef {
            name,
            kind,
            file_path,
            ..
        } => {
            serde_json::json!({"name": name, "kind": kind, "filePath": file_path})
        }
        EdgeRef {
            rel_type,
            confidence,
            reason,
            ..
        } => {
            serde_json::json!({"rel_type": format!("{rel_type:?}"), "confidence": confidence, "reason": reason})
        }
    }
}

fn serialize_toon(r: &cypher::QueryResult) -> String {
    let mut out = String::new();
    out.push_str(&format!("columns: {}\n", r.columns.join(", ")));
    out.push_str(&format!("rows[{}]:\n", r.rows.len()));
    for row in &r.rows {
        out.push_str("  ");
        let cells: Vec<String> = row.iter().map(value_to_toon).collect();
        out.push_str(&cells.join(", "));
        out.push('\n');
    }
    out
}

fn value_to_toon(v: &cypher::Value) -> String {
    use cypher::Value::*;
    match v {
        Null => "null".into(),
        Bool(b) => b.to_string(),
        Int(i) => i.to_string(),
        Float(f) => f.to_string(),
        Str(s) => s.clone(),
        List(xs) => format!(
            "[{}]",
            xs.iter().map(value_to_toon).collect::<Vec<_>>().join(",")
        ),
        NodeRef { name, kind, .. } => format!("{name}:{kind}"),
        EdgeRef {
            rel_type,
            confidence,
            ..
        } => format!("{rel_type:?}:{confidence}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::cypher::{QueryResult, Value};

    #[test]
    fn json_serialization_shape() {
        let r = QueryResult {
            columns: vec!["a.name".into(), "n".into()],
            rows: vec![vec![Value::Str("caller".into()), Value::Int(3)]],
        };
        let s = serialize_json(&r);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["columns"], serde_json::json!(["a.name", "n"]));
        assert_eq!(v["rows"][0][0], "caller");
        assert_eq!(v["rows"][0][1], 3);
    }

    #[test]
    fn toon_serialization_shape() {
        let r = QueryResult {
            columns: vec!["a.name".into(), "n".into()],
            rows: vec![
                vec![Value::Str("caller".into()), Value::Int(3)],
                vec![Value::Str("foo".into()), Value::Int(1)],
            ],
        };
        let s = serialize_toon(&r);
        assert!(s.contains("columns: a.name, n"));
        assert!(s.contains("rows[2]:"));
        assert!(s.contains("caller, 3"));
        assert!(s.contains("foo, 1"));
    }
}
