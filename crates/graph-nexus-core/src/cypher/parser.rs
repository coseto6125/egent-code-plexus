use crate::cypher::ast::*;
use crate::cypher::error::CypherError;
use crate::cypher::lexer::Token;
use crate::graph::{NodeKind, RelType};

pub struct Cursor<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    pub fn advance(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(t)
    }

    pub fn check(&self, want: &Token) -> bool {
        matches!(self.peek(), Some(t) if std::mem::discriminant(t) == std::mem::discriminant(want))
    }

    pub fn eat(&mut self, want: &Token) -> bool {
        if self.check(want) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, want: &Token) -> Result<(), CypherError> {
        if self.eat(want) {
            Ok(())
        } else {
            Err(self.err(format!("{want:?}")))
        }
    }

    pub fn err(&self, expected: impl Into<String>) -> CypherError {
        CypherError::Parse {
            offset: self.pos,
            expected: expected.into(),
            found: format!("{:?}", self.peek()),
        }
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }
}

pub fn parse_query(tokens: &[Token]) -> Result<Query, CypherError> {
    let mut c = Cursor::new(tokens);
    let q = parse_single_query(&mut c)?;
    if !c.at_end() {
        return Err(c.err("end of input"));
    }
    Ok(q)
}

fn parse_single_query(_c: &mut Cursor) -> Result<Query, CypherError> {
    // Filled out in B10.
    Err(CypherError::Parse {
        offset: 0,
        expected: "MATCH".into(),
        found: "stub".into(),
    })
}
