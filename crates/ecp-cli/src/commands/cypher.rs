use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use crate::repo_selector;
use clap::Args;
use ecp_core::cypher;
use ecp_core::registry::RegistryFile;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct CypherArgs {
    /// The Cypher query string. Supports a read-only subset of openCypher:
    ///
    /// - Multi-hop patterns: (a)-[:Calls]->(b)-[:Calls]->(c)
    /// - Variable-length:    (a)-[:Calls*1..3]->(b)
    /// - Label alternation:  (a:Function|Method)
    /// - WHERE:              =, <>, <, <=, >, >=, AND, OR, NOT, IN, =~, CONTAINS, STARTS WITH, ENDS WITH
    /// - Properties (node):  name, kind, filePath, uid, ownerClass, content,
    ///   is_test, is_async, is_static, is_abstract, is_generator, is_extern,
    ///   visibility, decorators
    /// - Properties (edge):  confidence, reason, rel_type
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

    /// Output format. Omit for the LLM-tuned default; explicit `--format
    /// toon|json|text` for the neutral / round-trippable / human paths.
    #[arg(long)]
    pub format: Option<String>,
}

impl CypherArgs {
    fn resolved_query(&self) -> Result<&str, ecp_core::EcpError> {
        self.query
            .as_deref()
            .or(self.query_positional.as_deref())
            .ok_or_else(|| {
                ecp_core::EcpError::InvalidArgument(
                    "cypher requires a query — pass it positionally (ecp cypher \"MATCH ...\") or via --query".into(),
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

pub fn run(args: CypherArgs, engine: &Engine) -> Result<(), ecp_core::EcpError> {
    // Multi-repo gate: cypher is single-repo only (graph identity is per-repo).
    if let Some(repo_sel) = args.repo.as_deref() {
        let home_ecp = ecp_core::registry::resolve_home_ecp();
        let registry = RegistryFile::read_or_empty(&home_ecp.join("registry.json"))
            .map_err(|e| ecp_core::EcpError::InvalidArgument(format!("registry read: {e}")))?;
        let selector = repo_selector::parse(repo_sel)
            .map_err(|e| ecp_core::EcpError::InvalidArgument(format!("--repo selector: {e}")))?;
        let cwd = std::env::current_dir().unwrap_or_default();
        let repos = repo_selector::resolve(&selector, &registry, cwd.to_str().unwrap_or("."))
            .map_err(|e| ecp_core::EcpError::InvalidArgument(format!("--repo: {e}")))?;
        if repos.len() > 1 {
            return Err(ecp_core::EcpError::InvalidArgument(format!(
                "cypher is single-repo only (graph identity); --repo resolved to {} repos. Pick one with --repo <name|path>.",
                repos.len()
            )));
        }
    }

    let graph = engine
        .graph()
        .map_err(|e| ecp_core::EcpError::Rkyv(e.to_string()))?;

    let query_str = args.resolved_query()?;
    let query = cypher::parse(query_str)
        .map_err(|e| ecp_core::EcpError::InvalidArgument(format_cypher_error(query_str, &e)))?;

    let result = cypher::execute(&query, graph, &resolve_repo_root(args.repo.as_deref()))
        .map_err(|e| ecp_core::EcpError::InvalidArgument(format_cypher_error(query_str, &e)))?;

    let rows_json: Vec<Vec<serde_json::Value>> = result
        .rows
        .iter()
        .map(|row| row.iter().map(value_to_json_value).collect())
        .collect();
    let payload = build_payload(result.columns, rows_json);
    emit(&payload, OutputFormat::parse(args.format.as_deref()))?;
    Ok(())
}

/// Wrap the cypher result into the emitted JSON shape, collapsing
/// `rows: [[a], [b]]` into `rows: [a, b]` when there's exactly one
/// projected column. The reader can still tell rows are scalars from
/// `columns.len() == 1`; the saving is one nesting level (plus a
/// disorienting `[1]:` toon prefix per row).
///
/// The executor SHOULD emit `row.len() == columns.len()` for every row,
/// but degenerate empty rows have historically been tolerated and the
/// unit test `build_payload_single_column_empty_row_yields_null` pins
/// the null-fallback contract. An LLM that sees `null` cannot tell a
/// legitimate null projection (`OPTIONAL MATCH` miss) apart from this
/// defensive fallback — so emit a stderr warning whenever the fallback
/// fires. The JSON shape stays flat null for backwards compatibility.
fn build_payload(columns: Vec<String>, rows: Vec<Vec<serde_json::Value>>) -> serde_json::Value {
    let rows_json: Vec<serde_json::Value> = if columns.len() == 1 {
        rows.into_iter()
            .map(|mut row| match row.pop() {
                Some(v) => v,
                None => {
                    eprintln!(
                        "warning: cypher executor returned empty row for single-column projection — surfacing as null"
                    );
                    serde_json::Value::Null
                }
            })
            .collect()
    } else {
        rows.into_iter().map(serde_json::Value::Array).collect()
    };
    serde_json::json!({ "columns": columns, "rows": rows_json })
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

fn value_to_json_value(v: &cypher::Value) -> serde_json::Value {
    use cypher::Value::*;
    match v {
        Null => serde_json::Value::Null,
        Bool(b) => serde_json::json!(b),
        Int(i) => serde_json::json!(i),
        Float(f) => serde_json::json!(f),
        Str(s) => serde_json::json!(s),
        List(xs) => serde_json::Value::Array(xs.iter().map(value_to_json_value).collect()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_payload_single_column_flattens_rows_to_scalars() {
        let columns = vec!["n.name".to_string()];
        let rows = vec![
            vec![json!("alpha")],
            vec![json!("beta")],
            vec![json!("gamma")],
        ];
        let payload = build_payload(columns, rows);
        assert_eq!(payload["columns"], json!(["n.name"]));
        // Single-column projection: rows are scalars, not 1-element arrays.
        assert_eq!(payload["rows"], json!(["alpha", "beta", "gamma"]));
    }

    #[test]
    fn build_payload_multi_column_preserves_row_arrays() {
        let columns = vec!["n.name".to_string(), "n.kind".to_string()];
        let rows = vec![
            vec![json!("alpha"), json!("Function")],
            vec![json!("Beta"), json!("Class")],
        ];
        let payload = build_payload(columns, rows);
        assert_eq!(payload["columns"], json!(["n.name", "n.kind"]));
        assert_eq!(
            payload["rows"],
            json!([["alpha", "Function"], ["Beta", "Class"]])
        );
    }

    #[test]
    fn build_payload_single_column_empty_row_yields_null() {
        // Defensive: a degenerate single-column row with no value still
        // emits Null rather than panicking.
        let payload = build_payload(vec!["x".to_string()], vec![vec![]]);
        assert_eq!(payload["rows"], json!([null]));
    }
}
