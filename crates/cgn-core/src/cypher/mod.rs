//! Cypher subset for read-only graph queries. See
//! `docs/specs/2026-05-16-cypher-expansion-design.md`.

pub mod ast;
pub mod error;
pub mod executor;
pub mod lexer;
pub mod parser;
pub mod value;

pub use ast::Query;
pub use error::CypherError;
pub use value::{QueryResult, Value};

use crate::graph::ArchivedZeroCopyGraph;
use std::path::Path;

/// Parse a Cypher query string into an AST.
pub fn parse(input: &str) -> Result<Query, CypherError> {
    let tokens = lexer::tokenize(input)?;
    parser::parse_query(&tokens)
}

/// Execute a parsed query against a graph. `repo_root` is used only for
/// `.content` projection (lazy file read).
pub fn execute(
    query: &Query,
    graph: &ArchivedZeroCopyGraph,
    repo_root: &Path,
) -> Result<QueryResult, CypherError> {
    executor::execute(query, graph, repo_root)
}
