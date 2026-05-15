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

pub fn parse_node_pat(c: &mut Cursor) -> Result<NodePat, CypherError> {
    c.expect(&Token::LParen)?;
    let var = if let Some(Token::Ident(_)) = c.peek() {
        if let Token::Ident(s) = c.advance().unwrap() { Some(s.clone()) } else { unreachable!() }
    } else { None };

    let mut kinds = Vec::new();
    if c.eat(&Token::Colon) {
        kinds.push(parse_node_kind(c)?);
        while c.eat(&Token::Pipe) {
            kinds.push(parse_node_kind(c)?);
        }
    }

    let mut props = Vec::new();
    if c.eat(&Token::LBrace) {
        loop {
            if c.eat(&Token::RBrace) { break; }
            let key = match c.advance() {
                Some(Token::Ident(s)) => s.clone(),
                _ => return Err(c.err("property name")),
            };
            c.expect(&Token::Colon)?;
            let lit = parse_literal(c)?;
            props.push((key, lit));
            if !c.eat(&Token::Comma) { c.expect(&Token::RBrace)?; break; }
        }
    }

    c.expect(&Token::RParen)?;
    Ok(NodePat { var, kinds, props })
}

pub fn parse_rel_pat(c: &mut Cursor) -> Result<RelPat, CypherError> {
    c.expect(&Token::LBracket)?;
    let var = if let Some(Token::Ident(_)) = c.peek() {
        if let Token::Ident(s) = c.advance().unwrap() { Some(s.clone()) } else { unreachable!() }
    } else { None };

    let mut types = Vec::new();
    if c.eat(&Token::Colon) {
        types.push(parse_rel_type(c)?);
        while c.eat(&Token::Pipe) {
            types.push(parse_rel_type(c)?);
        }
    }

    let range = if c.eat(&Token::Star) {
        let min = if let Some(Token::Int(n)) = c.peek() { let v = *n as u32; c.advance(); v } else { 1 };
        let max = if c.eat(&Token::DotDot) {
            if let Some(Token::Int(n)) = c.peek() { let v = *n as u32; c.advance(); v } else { u32::MAX }
        } else { min };
        Some((min, max))
    } else { None };

    c.expect(&Token::RBracket)?;
    // Direction is set by parse_pattern based on surrounding arrows.
    Ok(RelPat { var, types, range, dir: Direction::Out })
}

fn parse_node_kind(c: &mut Cursor) -> Result<NodeKind, CypherError> {
    let name = match c.advance() {
        Some(Token::Ident(s)) => s.clone(),
        _ => return Err(c.err("NodeKind ident")),
    };
    name.parse::<NodeKind>()
        .map_err(|_| CypherError::Semantic { msg: format!("unknown NodeKind '{name}'") })
}

fn parse_rel_type(c: &mut Cursor) -> Result<RelType, CypherError> {
    let name = match c.advance() {
        Some(Token::Ident(s)) => s.clone(),
        _ => return Err(c.err("RelType ident")),
    };
    // RelType::FromStr expects UPPER_SNAKE_CASE. Convert CamelCase → UPPER_SNAKE.
    let snake = camel_to_upper_snake(&name);
    snake.parse::<RelType>()
        .map_err(|_| CypherError::Semantic { msg: format!("unknown RelType '{name}'") })
}

/// Convert `HasMethod` → `HAS_METHOD` so Cypher CamelCase rel-types map to
/// the RelType::FromStr matcher which uses UPPER_SNAKE_CASE strings.
fn camel_to_upper_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

fn parse_literal(c: &mut Cursor) -> Result<Literal, CypherError> {
    match c.advance() {
        Some(Token::Null)     => Ok(Literal::Null),
        Some(Token::True)     => Ok(Literal::Bool(true)),
        Some(Token::False)    => Ok(Literal::Bool(false)),
        Some(Token::Int(n))   => Ok(Literal::Int(*n)),
        Some(Token::Float(f)) => Ok(Literal::Float(*f)),
        Some(Token::Str(s))   => Ok(Literal::Str(s.clone())),
        Some(Token::LBracket) => {
            let mut items = Vec::new();
            if !c.check(&Token::RBracket) {
                loop {
                    items.push(parse_literal(c)?);
                    if !c.eat(&Token::Comma) { break; }
                }
            }
            c.expect(&Token::RBracket)?;
            Ok(Literal::List(items))
        }
        _ => Err(c.err("literal")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cypher::lexer::tokenize;

    fn np(s: &str) -> NodePat {
        let toks = tokenize(s).unwrap();
        let mut c = Cursor::new(&toks);
        parse_node_pat(&mut c).unwrap()
    }
    fn rp(s: &str) -> RelPat {
        let toks = tokenize(s).unwrap();
        let mut c = Cursor::new(&toks);
        parse_rel_pat(&mut c).unwrap()
    }

    #[test]
    fn node_with_var_and_label() {
        let n = np("(a:Function)");
        assert_eq!(n.var.as_deref(), Some("a"));
        assert_eq!(n.kinds, vec![NodeKind::Function]);
    }

    #[test]
    fn node_with_label_alternation() {
        let n = np("(b:Function|Method)");
        assert_eq!(n.kinds, vec![NodeKind::Function, NodeKind::Method]);
    }

    #[test]
    fn node_anonymous() {
        let n = np("()");
        assert!(n.var.is_none());
        assert!(n.kinds.is_empty());
    }

    #[test]
    fn node_inline_props() {
        let n = np("(a:Function {name: 'foo'})");
        assert_eq!(n.props, vec![("name".into(), Literal::Str("foo".into()))]);
    }

    #[test]
    fn rel_default() {
        let r = rp("[r:Calls]");
        assert_eq!(r.var.as_deref(), Some("r"));
        assert_eq!(r.types, vec![RelType::Calls]);
        assert!(r.range.is_none());
    }

    #[test]
    fn rel_variable_length() {
        let r = rp("[*1..3]");
        assert_eq!(r.range, Some((1, 3)));
    }

    #[test]
    fn rel_type_alternation() {
        let r = rp("[:Calls|HasMethod]");
        assert_eq!(r.types, vec![RelType::Calls, RelType::HasMethod]);
    }
}
