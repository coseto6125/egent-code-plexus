use crate::cypher::ast::Query;
use crate::cypher::error::CypherError;
use crate::cypher::value::QueryResult;
use crate::graph::ArchivedZeroCopyGraph;
use std::path::Path;

pub fn execute(_query: &Query, _graph: &ArchivedZeroCopyGraph, _repo_root: &Path) -> Result<QueryResult, CypherError> {
    Err(CypherError::Exec { msg: "executor not yet implemented".into() })
}
