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

pub fn parse_match_clause(c: &mut Cursor) -> Result<MatchClause, CypherError> {
    let optional = c.eat(&Token::Optional);
    c.expect(&Token::Match)?;
    let mut patterns = vec![parse_pattern(c)?];
    while c.eat(&Token::Comma) {
        patterns.push(parse_pattern(c)?);
    }
    Ok(MatchClause { optional, patterns })
}

pub fn parse_pattern(c: &mut Cursor) -> Result<Pattern, CypherError> {
    let mut nodes = vec![parse_node_pat(c)?];
    let mut rels = Vec::new();

    while c.check(&Token::Dash) || c.check(&Token::RevArrow) {
        // Left side of the relationship
        let left_in = c.eat(&Token::RevArrow);
        if !left_in { c.expect(&Token::Dash)?; }

        // Optional bracketed rel
        let mut rel = if c.check(&Token::LBracket) {
            parse_rel_pat(c)?
        } else {
            RelPat { var: None, types: Vec::new(), range: None, dir: Direction::Out }
        };

        // Right side
        let right_out = c.eat(&Token::Arrow);
        if !right_out { c.expect(&Token::Dash)?; }

        rel.dir = match (left_in, right_out) {
            (false, true)  => Direction::Out,
            (true,  false) => Direction::In,
            (false, false) => Direction::Both,
            (true,  true)  => return Err(CypherError::Parse {
                offset: c.pos,
                expected: "single-direction arrow".into(),
                found: "<- and -> both".into(),
            }),
        };

        rels.push(rel);
        nodes.push(parse_node_pat(c)?);
    }
    Ok(Pattern { nodes, rels })
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

pub fn parse_expr(c: &mut Cursor) -> Result<Expr, CypherError> {
    parse_or(c)
}

fn parse_or(c: &mut Cursor) -> Result<Expr, CypherError> {
    let mut lhs = parse_and(c)?;
    while c.eat(&Token::Or) {
        let rhs = parse_and(c)?;
        lhs = Expr::BinOp(Op::Or, Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn parse_and(c: &mut Cursor) -> Result<Expr, CypherError> {
    let mut lhs = parse_not(c)?;
    while c.eat(&Token::And) {
        let rhs = parse_not(c)?;
        lhs = Expr::BinOp(Op::And, Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn parse_not(c: &mut Cursor) -> Result<Expr, CypherError> {
    if c.eat(&Token::Not) {
        let inner = parse_not(c)?;
        Ok(Expr::UnaryOp(UnaryOp::Not, Box::new(inner)))
    } else {
        parse_comparison(c)
    }
}

fn parse_comparison(c: &mut Cursor) -> Result<Expr, CypherError> {
    let lhs = parse_primary(c)?;

    // Postfix-style operators
    if c.eat(&Token::In) {
        c.expect(&Token::LBracket)?;
        let mut items = Vec::new();
        if !c.check(&Token::RBracket) {
            loop {
                items.push(parse_literal(c)?);
                if !c.eat(&Token::Comma) { break; }
            }
        }
        c.expect(&Token::RBracket)?;
        return Ok(Expr::In(Box::new(lhs), items));
    }
    if c.eat(&Token::RegexMatch) {
        let pat = match c.advance() {
            Some(Token::Str(s)) => s.clone(),
            _ => return Err(c.err("regex string literal after =~")),
        };
        return Ok(Expr::Regex(Box::new(lhs), pat));
    }
    if c.eat(&Token::StartsWith) {
        let s = match c.advance() {
            Some(Token::Str(s)) => s.clone(),
            _ => return Err(c.err("string after STARTS WITH")),
        };
        return Ok(Expr::StartsWith(Box::new(lhs), s));
    }
    if c.eat(&Token::EndsWith) {
        let s = match c.advance() {
            Some(Token::Str(s)) => s.clone(),
            _ => return Err(c.err("string after ENDS WITH")),
        };
        return Ok(Expr::EndsWith(Box::new(lhs), s));
    }
    if c.eat(&Token::Contains) {
        let s = match c.advance() {
            Some(Token::Str(s)) => s.clone(),
            _ => return Err(c.err("string after CONTAINS")),
        };
        return Ok(Expr::Contains(Box::new(lhs), s));
    }

    // Infix binary comparisons
    let op = if c.eat(&Token::Eq)      { Some(Op::Eq) }
        else if c.eat(&Token::Ne)      { Some(Op::Ne) }
        else if c.eat(&Token::Lt)      { Some(Op::Lt) }
        else if c.eat(&Token::Le)      { Some(Op::Le) }
        else if c.eat(&Token::Gt)      { Some(Op::Gt) }
        else if c.eat(&Token::Ge)      { Some(Op::Ge) }
        else                           { None };

    if let Some(op) = op {
        let rhs = parse_primary(c)?;
        Ok(Expr::BinOp(op, Box::new(lhs), Box::new(rhs)))
    } else {
        Ok(lhs)
    }
}

fn parse_primary(c: &mut Cursor) -> Result<Expr, CypherError> {
    if c.eat(&Token::LParen) {
        let e = parse_expr(c)?;
        c.expect(&Token::RParen)?;
        return Ok(e);
    }
    // Property access `ident.ident` OR function call `IDENT(...)`.
    if let Some(Token::Ident(name)) = c.peek().cloned() {
        c.advance();
        if c.eat(&Token::Dot) {
            let prop = match c.advance() {
                Some(Token::Ident(s)) => s.clone(),
                _ => return Err(c.err("property name after .")),
            };
            return Ok(Expr::Prop(name, prop));
        }
        if c.eat(&Token::LParen) {
            let distinct = c.eat(&Token::Distinct);
            if c.eat(&Token::Star) {
                // COUNT(*): zero args sentinel via Null literal.
                c.expect(&Token::RParen)?;
                return Ok(Expr::FunCall {
                    name: name.to_ascii_uppercase(),
                    distinct: false,
                    args: vec![Expr::Lit(Literal::Null)],
                });
            }
            let mut args = Vec::new();
            if !c.check(&Token::RParen) {
                loop {
                    args.push(parse_expr(c)?);
                    if !c.eat(&Token::Comma) { break; }
                }
            }
            c.expect(&Token::RParen)?;
            return Ok(Expr::FunCall { name: name.to_ascii_uppercase(), distinct, args });
        }
        return Err(c.err("`.<prop>` or `(...)` after identifier"));
    }
    // Literal
    let lit = parse_literal(c)?;
    Ok(Expr::Lit(lit))
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

    fn pat(s: &str) -> Pattern {
        let toks = tokenize(s).unwrap();
        let mut c = Cursor::new(&toks);
        parse_pattern(&mut c).unwrap()
    }

    #[test]
    fn pattern_single_hop_out() {
        let p = pat("(a:Function)-[r:Calls]->(b:Function)");
        assert_eq!(p.nodes.len(), 2);
        assert_eq!(p.rels.len(), 1);
        assert_eq!(p.rels[0].dir, Direction::Out);
    }

    #[test]
    fn pattern_reverse_arrow() {
        let p = pat("(a)<-[:Calls]-(b)");
        assert_eq!(p.rels[0].dir, Direction::In);
    }

    #[test]
    fn pattern_undirected() {
        let p = pat("(a)-[:Calls]-(b)");
        assert_eq!(p.rels[0].dir, Direction::Both);
    }

    #[test]
    fn pattern_three_hops() {
        let p = pat("(a)-[:Calls]->(b)-[:Calls]->(c)-[:Calls]->(d)");
        assert_eq!(p.nodes.len(), 4);
        assert_eq!(p.rels.len(), 3);
    }

    #[test]
    fn pattern_anonymous_rel() {
        let p = pat("(a)-->(b)");
        assert_eq!(p.rels.len(), 1);
        assert!(p.rels[0].types.is_empty());
        assert_eq!(p.rels[0].dir, Direction::Out);
    }

    fn mc(s: &str) -> MatchClause {
        let toks = tokenize(s).unwrap();
        let mut c = Cursor::new(&toks);
        parse_match_clause(&mut c).unwrap()
    }

    #[test]
    fn match_single_pattern() {
        let m = mc("MATCH (a)-[:Calls]->(b)");
        assert!(!m.optional);
        assert_eq!(m.patterns.len(), 1);
    }

    #[test]
    fn match_multiple_patterns_comma() {
        let m = mc("MATCH (a)-[:Calls]->(b), (c)-[:HasMethod]->(d)");
        assert_eq!(m.patterns.len(), 2);
    }

    #[test]
    fn match_optional() {
        let m = mc("OPTIONAL MATCH (a)-[:Calls]->(b)");
        assert!(m.optional);
    }

    fn ex(s: &str) -> Expr {
        let toks = tokenize(s).unwrap();
        let mut c = Cursor::new(&toks);
        parse_expr(&mut c).unwrap()
    }

    #[test]
    fn expr_property_eq_string() {
        match ex("a.name = 'foo'") {
            Expr::BinOp(Op::Eq, lhs, rhs) => {
                assert!(matches!(*lhs, Expr::Prop(ref v, ref p) if v == "a" && p == "name"));
                assert!(matches!(*rhs, Expr::Lit(Literal::Str(ref s)) if s == "foo"));
            }
            other => panic!("expected BinOp(Eq, ...), got {other:?}"),
        }
    }

    #[test]
    fn expr_and_or_precedence() {
        // a=1 AND b=2 OR c=3  →  (a=1 AND b=2) OR c=3
        match ex("a.x = 1 AND b.y = 2 OR c.z = 3") {
            Expr::BinOp(Op::Or, lhs, _) => {
                assert!(matches!(*lhs, Expr::BinOp(Op::And, ..)));
            }
            _ => panic!("expected Or as root"),
        }
    }

    #[test]
    fn expr_not_unary() {
        match ex("NOT a.name = 'x'") {
            Expr::UnaryOp(UnaryOp::Not, _) => {}
            _ => panic!("expected Not"),
        }
    }

    #[test]
    fn expr_in_list() {
        match ex("a.kind IN ['Function', 'Method']") {
            Expr::In(_, lits) => assert_eq!(lits.len(), 2),
            _ => panic!("expected In"),
        }
    }

    #[test]
    fn expr_starts_with() {
        match ex("a.name STARTS WITH 'foo'") {
            Expr::StartsWith(_, s) => assert_eq!(s, "foo"),
            _ => panic!("expected StartsWith"),
        }
    }

    #[test]
    fn expr_regex_match() {
        match ex("a.name =~ '.*Handler$'") {
            Expr::Regex(_, s) => assert_eq!(s, ".*Handler$"),
            _ => panic!("expected Regex"),
        }
    }

    #[test]
    fn expr_paren() {
        match ex("(a.x = 1 OR b.y = 2) AND c.z = 3") {
            Expr::BinOp(Op::And, lhs, _) => {
                assert!(matches!(*lhs, Expr::BinOp(Op::Or, ..)));
            }
            _ => panic!("expected And as root"),
        }
    }
}
