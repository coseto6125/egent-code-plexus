use crate::cypher::ast::Query;
use crate::cypher::error::CypherError;
use crate::cypher::lexer::Token;

pub fn parse_query(_tokens: &[Token]) -> Result<Query, CypherError> {
    Err(CypherError::Parse { offset: 0, expected: "_".into(), found: "_".into() })
}
