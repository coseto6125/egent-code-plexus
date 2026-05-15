use crate::cypher::error::CypherError;

#[derive(Debug, Clone, PartialEq)]
pub enum Token { /* filled in Task A2 */ Placeholder }

pub fn tokenize(_input: &str) -> Result<Vec<Token>, CypherError> {
    Err(CypherError::Lex { offset: 0, msg: "lexer not yet implemented".into() })
}
