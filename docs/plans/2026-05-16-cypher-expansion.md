# Cypher Expansion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the regex-based `cgn cypher` minimal-pattern parser with a proper recursive-descent Cypher parser + executor under `cgn-core/cypher/`, supporting multi-hop chains, OPTIONAL MATCH, rich WHERE, aggregation, DISTINCT/ORDER/LIMIT, UNION, and column-based output (JSON default + TOON option).

**Architecture:** New `crates/cgn-core/src/cypher/` module owns lexer → parser → AST → executor → `QueryResult { columns, rows }`. CLI shim in `crates/cgn-cli/src/commands/cypher.rs` reduces to argument parsing + serialization (JSON / TOON). All existing cypher e2e tests rewrite to the new column shape.

**Tech Stack:** Rust 2021, no new deps (zero external parser crate), serde_json (existing), regex (existing — used by `=~` operator at exec time).

**Spec reference:** `docs/specs/2026-05-16-cypher-expansion-design.md`

---

## API Conventions Used in This Plan

Where this plan writes `graph.nodes[idx]` / `graph.out_offsets` / `graph.edges[i]`, the real API is the rkyv-archived view from `cgn_core::graph::ArchivedZeroCopyGraph` — slice indexing already works since `rkyv` makes archived `Vec<T>` slice-indexable. `node.name.resolve(&graph.string_pool)` is the canonical way to get a string out of `StrRef`. `NodeKind` / `RelType` `.parse()` already implements `FromStr` (used by current cypher.rs).

`CgnError` variants referenced (`InvalidArgument`, `Rkyv`): see `crates/cgn-core/src/error.rs`. Wrap CypherError as `CgnError::InvalidArgument(format!("cypher: {e}"))` at the CLI boundary.

`cgn_core::graph_query::callees_of` / `callers_of` already exist — Task C4 generalizes them.

The `BindingValue` and `Bindings` types defined in Task C1 are used across Tasks C2–C12. Re-read C1 if a later task references them.

---

## Pre-flight

This worktree (`feat+cypher-expansion`) was created from main `7d7529d` and the design doc was committed in `306fec6`. The workspace builds clean as-is.

```bash
cargo build --workspace
cargo test --workspace --no-run
```

Both must pass before starting Task A1.

---

## File Structure

**Create:**
- `crates/cgn-core/src/cypher/mod.rs` — public API: `parse`, `execute`, `Query`, `QueryResult`, `CypherError`
- `crates/cgn-core/src/cypher/lexer.rs` — `Token`, `tokenize(&str) -> Result<Vec<Token>>`
- `crates/cgn-core/src/cypher/ast.rs` — AST types (`Query`, `MatchClause`, `Pattern`, `NodePat`, `RelPat`, `Expr`, …)
- `crates/cgn-core/src/cypher/parser.rs` — recursive-descent parser
- `crates/cgn-core/src/cypher/executor.rs` — `execute(&Query, &Graph, &PathBuf) -> Result<QueryResult>`
- `crates/cgn-core/src/cypher/value.rs` — `Value`, `QueryResult`
- `crates/cgn-core/src/cypher/error.rs` — `CypherError`
- `crates/cgn-cli/tests/cypher_multi_hop.rs` — new e2e
- `crates/cgn-cli/tests/cypher_aggregation.rs` — new e2e
- `crates/cgn-cli/tests/cypher_toon_format.rs` — new e2e
- `crates/cgn-cli/tests/cypher_error_messages.rs` — new e2e

**Modify:**
- `crates/cgn-core/src/lib.rs` — `pub mod cypher;`
- `crates/cgn-cli/src/commands/cypher.rs` — replace body with thin wrapper (~50 LoC)
- `crates/cgn-cli/tests/cypher_content.rs` — assertions migrate to `{columns, rows}`
- `crates/cgn-cli/tests/context_cypher_edge_metadata.rs` — same

---

## Phase A — Module scaffolding + lexer

### Task A1: Create cypher module skeleton + register in lib.rs

**Files:**
- Create: `crates/cgn-core/src/cypher/mod.rs`
- Create: `crates/cgn-core/src/cypher/value.rs`
- Create: `crates/cgn-core/src/cypher/error.rs`
- Modify: `crates/cgn-core/src/lib.rs`

- [ ] **Step 1: Add module declaration to lib.rs**

Open `crates/cgn-core/src/lib.rs` and after line 7 (`pub mod graph_query;`) add:

```rust
pub mod cypher;
```

- [ ] **Step 2: Create cypher/mod.rs with public API surface**

```rust
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
```

- [ ] **Step 3: Create value.rs stub**

```rust
use crate::graph::RelType;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    /// Reference to a graph node. CLI side resolves `.name`/`.kind`/`.filePath`
    /// for human-readable serialization.
    NodeRef { idx: u32, name: String, kind: String, file_path: String },
    EdgeRef { src: u32, tgt: u32, rel_type: RelType, confidence: f32, reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}
```

- [ ] **Step 4: Create error.rs stub**

```rust
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum CypherError {
    Lex      { offset: usize, msg: String },
    Parse    { offset: usize, expected: String, found: String },
    Semantic { msg: String },
    Exec     { msg: String },
}

impl fmt::Display for CypherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lex      { offset, msg }              => write!(f, "lex error at byte {offset}: {msg}"),
            Self::Parse    { offset, expected, found }  => write!(f, "parse error at byte {offset}: expected {expected}, found {found}"),
            Self::Semantic { msg }                       => write!(f, "semantic error: {msg}"),
            Self::Exec     { msg }                       => write!(f, "execution error: {msg}"),
        }
    }
}

impl std::error::Error for CypherError {}
```

- [ ] **Step 5: Create empty stubs for the rest so cargo build compiles**

`crates/cgn-core/src/cypher/ast.rs`:

```rust
use crate::graph::{NodeKind, RelType};

#[derive(Debug, Clone)]
pub struct Query {
    pub matches: Vec<MatchClause>,
    pub where_: Option<Expr>,
    pub with: Option<WithClause>,
    pub return_: ReturnClause,
    pub order_by: Vec<OrderItem>,
    pub skip: Option<u64>,
    pub limit: Option<u64>,
    pub union: Option<Box<Query>>,
    pub union_all: bool,
}

#[derive(Debug, Clone)]
pub struct MatchClause { pub optional: bool, pub patterns: Vec<Pattern> }

#[derive(Debug, Clone)]
pub struct Pattern { pub nodes: Vec<NodePat>, pub rels: Vec<RelPat> }

#[derive(Debug, Clone)]
pub struct NodePat { pub var: Option<String>, pub kinds: Vec<NodeKind>, pub props: Vec<(String, Literal)> }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction { Out, In, Both }

#[derive(Debug, Clone)]
pub struct RelPat { pub var: Option<String>, pub types: Vec<RelType>, pub range: Option<(u32, u32)>, pub dir: Direction }

#[derive(Debug, Clone)]
pub enum Literal { Null, Bool(bool), Int(i64), Float(f64), Str(String), List(Vec<Literal>) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op { Eq, Ne, Lt, Le, Gt, Ge, And, Or }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp { Not }

#[derive(Debug, Clone)]
pub enum Expr {
    BinOp(Op, Box<Expr>, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    Prop(String, String),
    Lit(Literal),
    In(Box<Expr>, Vec<Literal>),
    Regex(Box<Expr>, String),
    StartsWith(Box<Expr>, String),
    EndsWith(Box<Expr>, String),
    Contains(Box<Expr>, String),
    FunCall { name: String, distinct: bool, args: Vec<Expr> },
}

#[derive(Debug, Clone)]
pub struct ReturnClause { pub distinct: bool, pub items: Vec<ReturnItem> }

#[derive(Debug, Clone)]
pub struct ReturnItem { pub expr: ReturnExpr, pub alias: Option<String> }

#[derive(Debug, Clone)]
pub enum ReturnExpr { Star, Var(String), Prop(String, String), FunCall { name: String, distinct: bool, args: Vec<Expr> } }

#[derive(Debug, Clone)]
pub struct OrderItem { pub expr: ReturnExpr, pub desc: bool }

#[derive(Debug, Clone)]
pub struct WithClause { pub items: Vec<ReturnItem>, pub where_: Option<Expr> }
```

`crates/cgn-core/src/cypher/lexer.rs`:

```rust
use crate::cypher::error::CypherError;

#[derive(Debug, Clone, PartialEq)]
pub enum Token { /* filled in Task A2 */ Placeholder }

pub fn tokenize(_input: &str) -> Result<Vec<Token>, CypherError> {
    Err(CypherError::Lex { offset: 0, msg: "lexer not yet implemented".into() })
}
```

`crates/cgn-core/src/cypher/parser.rs`:

```rust
use crate::cypher::ast::Query;
use crate::cypher::error::CypherError;
use crate::cypher::lexer::Token;

pub fn parse_query(_tokens: &[Token]) -> Result<Query, CypherError> {
    Err(CypherError::Parse { offset: 0, expected: "_".into(), found: "_".into() })
}
```

`crates/cgn-core/src/cypher/executor.rs`:

```rust
use crate::cypher::ast::Query;
use crate::cypher::error::CypherError;
use crate::cypher::value::QueryResult;
use crate::graph::ArchivedZeroCopyGraph;
use std::path::Path;

pub fn execute(_query: &Query, _graph: &ArchivedZeroCopyGraph, _repo_root: &Path) -> Result<QueryResult, CypherError> {
    Err(CypherError::Exec { msg: "executor not yet implemented".into() })
}
```

- [ ] **Step 6: Verify build**

Run: `cargo build -p cgn-core`
Expected: PASS (warnings about unused fields are OK).

- [ ] **Step 7: Commit**

```bash
git add crates/cgn-core/src/lib.rs crates/cgn-core/src/cypher/
git commit -m "feat(cypher): scaffold cypher module (ast/lexer/parser/executor stubs)"
```

---

### Task A2: Lexer — tokens + tests

**Files:**
- Modify: `crates/cgn-core/src/cypher/lexer.rs`
- Test: inline `#[cfg(test)] mod tests` inside `lexer.rs`

- [ ] **Step 1: Write failing tests (table-driven)**

Replace the placeholder at the end of `lexer.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> Vec<Token> {
        tokenize(s).expect("lex ok")
    }

    #[test]
    fn keywords_case_insensitive() {
        for k in ["MATCH", "match", "Match"] {
            assert_eq!(lex(k), vec![Token::Match]);
        }
    }

    #[test]
    fn punctuation() {
        assert_eq!(
            lex("()[]{},.:"),
            vec![Token::LParen, Token::RParen, Token::LBracket, Token::RBracket,
                 Token::LBrace, Token::RBrace, Token::Comma, Token::Dot, Token::Colon]
        );
    }

    #[test]
    fn arrows_and_pipe() {
        assert_eq!(lex("->"), vec![Token::Arrow]);
        assert_eq!(lex("<-"), vec![Token::RevArrow]);
        assert_eq!(lex("-"), vec![Token::Dash]);
        assert_eq!(lex("|"), vec![Token::Pipe]);
        assert_eq!(lex("*"), vec![Token::Star]);
        assert_eq!(lex(".."), vec![Token::DotDot]);
    }

    #[test]
    fn comparison_operators() {
        assert_eq!(lex("= <> < <= > >= =~"),
            vec![Token::Eq, Token::Ne, Token::Lt, Token::Le, Token::Gt, Token::Ge, Token::RegexMatch]);
    }

    #[test]
    fn string_literal_single_and_double() {
        assert_eq!(lex("'hello'"), vec![Token::Str("hello".into())]);
        assert_eq!(lex(r#""hi""#), vec![Token::Str("hi".into())]);
        assert_eq!(lex(r"'it\'s'"), vec![Token::Str("it's".into())]);
    }

    #[test]
    fn numbers_int_and_float() {
        assert_eq!(lex("42"), vec![Token::Int(42)]);
        assert_eq!(lex("3.14"), vec![Token::Float(3.14)]);
        assert_eq!(lex("-7"), vec![Token::Dash, Token::Int(7)]);
    }

    #[test]
    fn identifiers_and_kinds() {
        assert_eq!(lex("foo Function r"),
            vec![Token::Ident("foo".into()), Token::Ident("Function".into()), Token::Ident("r".into())]);
    }

    #[test]
    fn whitespace_skipped() {
        assert_eq!(lex("MATCH    (a)"),
            vec![Token::Match, Token::LParen, Token::Ident("a".into()), Token::RParen]);
    }

    #[test]
    fn lex_error_unterminated_string() {
        let err = tokenize("'unclosed").unwrap_err();
        match err { CypherError::Lex { .. } => {}, e => panic!("expected Lex, got {e:?}") }
    }
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p cgn-core cypher::lexer`
Expected: FAIL — every test errors on placeholder `tokenize`.

- [ ] **Step 3: Implement Token enum + tokenize**

Replace the body of `lexer.rs` with:

```rust
use crate::cypher::error::CypherError;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Match, Optional, Where, Return, Distinct, With, As,
    OrderBy, Asc, Desc, Skip, Limit, Union, All,
    And, Or, Not, In, StartsWith, EndsWith, Contains,

    // Literals
    True, False, Null,
    Int(i64), Float(f64), Str(String),
    Ident(String),

    // Symbols
    LParen, RParen, LBracket, RBracket, LBrace, RBrace,
    Comma, Dot, DotDot, Colon, Pipe, Star, Dash,
    Arrow, RevArrow,
    Eq, Ne, Lt, Le, Gt, Ge, RegexMatch,
}

pub fn tokenize(input: &str) -> Result<Vec<Token>, CypherError> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() { i += 1; continue; }

        // Multi-char symbols first
        if c == b'-' && bytes.get(i + 1) == Some(&b'>') { out.push(Token::Arrow); i += 2; continue; }
        if c == b'<' && bytes.get(i + 1) == Some(&b'-') { out.push(Token::RevArrow); i += 2; continue; }
        if c == b'<' && bytes.get(i + 1) == Some(&b'=') { out.push(Token::Le); i += 2; continue; }
        if c == b'>' && bytes.get(i + 1) == Some(&b'=') { out.push(Token::Ge); i += 2; continue; }
        if c == b'<' && bytes.get(i + 1) == Some(&b'>') { out.push(Token::Ne); i += 2; continue; }
        if c == b'=' && bytes.get(i + 1) == Some(&b'~') { out.push(Token::RegexMatch); i += 2; continue; }
        if c == b'.' && bytes.get(i + 1) == Some(&b'.') { out.push(Token::DotDot); i += 2; continue; }

        match c {
            b'(' => { out.push(Token::LParen);   i += 1; continue; }
            b')' => { out.push(Token::RParen);   i += 1; continue; }
            b'[' => { out.push(Token::LBracket); i += 1; continue; }
            b']' => { out.push(Token::RBracket); i += 1; continue; }
            b'{' => { out.push(Token::LBrace);   i += 1; continue; }
            b'}' => { out.push(Token::RBrace);   i += 1; continue; }
            b',' => { out.push(Token::Comma);    i += 1; continue; }
            b'.' => { out.push(Token::Dot);      i += 1; continue; }
            b':' => { out.push(Token::Colon);    i += 1; continue; }
            b'|' => { out.push(Token::Pipe);     i += 1; continue; }
            b'*' => { out.push(Token::Star);     i += 1; continue; }
            b'-' => { out.push(Token::Dash);     i += 1; continue; }
            b'=' => { out.push(Token::Eq);       i += 1; continue; }
            b'<' => { out.push(Token::Lt);       i += 1; continue; }
            b'>' => { out.push(Token::Gt);       i += 1; continue; }
            _ => {}
        }

        // String literal
        if c == b'\'' || c == b'"' {
            let quote = c;
            let start = i;
            i += 1;
            let mut s = String::new();
            loop {
                if i >= bytes.len() {
                    return Err(CypherError::Lex { offset: start, msg: "unterminated string".into() });
                }
                let b = bytes[i];
                if b == b'\\' && i + 1 < bytes.len() {
                    s.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                if b == quote { i += 1; break; }
                s.push(b as char);
                i += 1;
            }
            out.push(Token::Str(s));
            continue;
        }

        // Number
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
            let mut is_float = false;
            if i < bytes.len() && bytes[i] == b'.' && bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit()) {
                is_float = true;
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
            }
            let s = &input[start..i];
            let tok = if is_float {
                Token::Float(s.parse().map_err(|_| CypherError::Lex { offset: start, msg: format!("bad float {s}") })?)
            } else {
                Token::Int(s.parse().map_err(|_| CypherError::Lex { offset: start, msg: format!("bad int {s}") })?)
            };
            out.push(tok);
            continue;
        }

        // Identifier / keyword
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
            let s = &input[start..i];
            let tok = match s.to_ascii_uppercase().as_str() {
                "MATCH"    => Token::Match,
                "OPTIONAL" => Token::Optional,
                "WHERE"    => Token::Where,
                "RETURN"   => Token::Return,
                "DISTINCT" => Token::Distinct,
                "WITH"     => Token::With,
                "AS"       => Token::As,
                "ORDER"    => {
                    // expect BY
                    let save = i;
                    while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
                    let bs = i;
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() { i += 1; }
                    if input[bs..i].eq_ignore_ascii_case("BY") {
                        Token::OrderBy
                    } else { i = save; Token::Ident(s.into()) }
                }
                "ASC"      => Token::Asc,
                "DESC"     => Token::Desc,
                "SKIP"     => Token::Skip,
                "LIMIT"    => Token::Limit,
                "UNION"    => Token::Union,
                "ALL"      => Token::All,
                "AND"      => Token::And,
                "OR"       => Token::Or,
                "NOT"      => Token::Not,
                "IN"       => Token::In,
                "STARTS"   => {
                    let save = i;
                    while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
                    let bs = i;
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() { i += 1; }
                    if input[bs..i].eq_ignore_ascii_case("WITH") {
                        Token::StartsWith
                    } else { i = save; Token::Ident(s.into()) }
                }
                "ENDS"     => {
                    let save = i;
                    while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
                    let bs = i;
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() { i += 1; }
                    if input[bs..i].eq_ignore_ascii_case("WITH") {
                        Token::EndsWith
                    } else { i = save; Token::Ident(s.into()) }
                }
                "CONTAINS" => Token::Contains,
                "TRUE"     => Token::True,
                "FALSE"    => Token::False,
                "NULL"     => Token::Null,
                _          => Token::Ident(s.into()),
            };
            out.push(tok);
            continue;
        }

        return Err(CypherError::Lex { offset: i, msg: format!("unexpected byte {:?}", c as char) });
    }
    Ok(out)
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p cgn-core cypher::lexer`
Expected: PASS (9/9).

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-core/src/cypher/lexer.rs
git commit -m "feat(cypher): lexer with case-insensitive keywords + tests"
```

---

## Phase B — Parser (recursive descent, clause-by-clause TDD)

For Phase B, each task follows the same shape: tests first, then parser fn, then verify. The parser carries an index cursor `pos: usize` through `&[Token]`, with helpers `peek()`, `advance()`, `expect(Token)`, `error_at_pos(expected, found)`.

### Task B1: Parser scaffolding (cursor + helpers)

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Define cursor struct + helpers**

Replace `parser.rs` with:

```rust
use crate::cypher::ast::*;
use crate::cypher::error::CypherError;
use crate::cypher::lexer::Token;
use crate::graph::{NodeKind, RelType};

pub struct Cursor<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(tokens: &'a [Token]) -> Self { Self { tokens, pos: 0 } }

    pub fn peek(&self) -> Option<&Token> { self.tokens.get(self.pos) }

    pub fn advance(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(t)
    }

    pub fn check(&self, want: &Token) -> bool {
        matches!(self.peek(), Some(t) if std::mem::discriminant(t) == std::mem::discriminant(want))
    }

    pub fn eat(&mut self, want: &Token) -> bool {
        if self.check(want) { self.pos += 1; true } else { false }
    }

    pub fn expect(&mut self, want: &Token) -> Result<(), CypherError> {
        if self.eat(want) { Ok(()) }
        else { Err(self.err(format!("{want:?}"))) }
    }

    pub fn err(&self, expected: impl Into<String>) -> CypherError {
        CypherError::Parse {
            offset: self.pos,
            expected: expected.into(),
            found: format!("{:?}", self.peek()),
        }
    }

    pub fn at_end(&self) -> bool { self.pos >= self.tokens.len() }
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
    // Filled out in B11.
    Err(CypherError::Parse { offset: 0, expected: "MATCH".into(), found: "stub".into() })
}
```

- [ ] **Step 2: Verify build**

Run: `cargo build -p cgn-core`
Expected: PASS (warnings about unused functions are OK).

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parser cursor + helpers"
```

---

### Task B2: Parse NodePat + RelPat (the building blocks)

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Tests**

Append at the bottom of `parser.rs`:

```rust
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
        // Bare `-[r:Calls]->` style is parsed by `parse_pattern` later.
        // Here we test the inner bracket form `[r:Calls]`.
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
```

- [ ] **Step 2: Verify tests fail**

Run: `cargo test -p cgn-core cypher::parser`
Expected: FAIL — `parse_node_pat` / `parse_rel_pat` not defined.

- [ ] **Step 3: Implement parse_node_pat + parse_rel_pat + helpers**

Add to `parser.rs` (above the `#[cfg(test)]` block):

```rust
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
    name.parse::<RelType>()
        .map_err(|_| CypherError::Semantic { msg: format!("unknown RelType '{name}'") })
}

fn parse_literal(c: &mut Cursor) -> Result<Literal, CypherError> {
    match c.advance() {
        Some(Token::Null)      => Ok(Literal::Null),
        Some(Token::True)      => Ok(Literal::Bool(true)),
        Some(Token::False)     => Ok(Literal::Bool(false)),
        Some(Token::Int(n))    => Ok(Literal::Int(*n)),
        Some(Token::Float(f))  => Ok(Literal::Float(*f)),
        Some(Token::Str(s))    => Ok(Literal::Str(s.clone())),
        Some(Token::LBracket)  => {
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
```

Confirm `NodeKind::FromStr` and `RelType::FromStr` exist. They do — current `cypher.rs` already does `m.as_str().parse().ok()` on both at lines 252-254.

- [ ] **Step 4: Run tests**

Run: `cargo test -p cgn-core cypher::parser`
Expected: PASS (7/7).

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse NodePat + RelPat (label alternation, inline props, variable-length)"
```

---

### Task B3: Parse chained Pattern + arrow direction

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Tests**

Append inside `mod tests`:

```rust
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
```

- [ ] **Step 2: Verify failures**

Run: `cargo test -p cgn-core cypher::parser`
Expected: FAIL — `parse_pattern` not defined.

- [ ] **Step 3: Implement parse_pattern**

Add to `parser.rs` above `#[cfg(test)]`:

```rust
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
                offset: c.pos, expected: "single-direction arrow".into(),
                found: "<- and -> both".into(),
            }),
        };

        rels.push(rel);
        nodes.push(parse_node_pat(c)?);
    }
    Ok(Pattern { nodes, rels })
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cgn-core cypher::parser`
Expected: PASS (12/12 including B2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse chained patterns + arrow direction (Out/In/Both)"
```

---

### Task B4: Parse MATCH / OPTIONAL MATCH clause

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Tests**

```rust
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
```

- [ ] **Step 2: Verify failures**

Run: `cargo test -p cgn-core cypher::parser::tests::match_`
Expected: FAIL.

- [ ] **Step 3: Implement parse_match_clause**

```rust
pub fn parse_match_clause(c: &mut Cursor) -> Result<MatchClause, CypherError> {
    let optional = c.eat(&Token::Optional);
    c.expect(&Token::Match)?;
    let mut patterns = vec![parse_pattern(c)?];
    while c.eat(&Token::Comma) {
        patterns.push(parse_pattern(c)?);
    }
    Ok(MatchClause { optional, patterns })
}
```

- [ ] **Step 4: Run tests + commit**

Run: `cargo test -p cgn-core cypher::parser`
Expected: PASS (15/15).

```bash
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse MATCH / OPTIONAL MATCH clause"
```

---

### Task B5: Parse Expr — Pratt-style precedence climbing

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Tests**

```rust
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
```

- [ ] **Step 2: Verify failures**

Run: `cargo test -p cgn-core cypher::parser::tests::expr_`
Expected: FAIL.

- [ ] **Step 3: Implement parse_expr (Pratt)**

```rust
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
    let op = if c.eat(&Token::Eq) { Some(Op::Eq) }
        else if c.eat(&Token::Ne) { Some(Op::Ne) }
        else if c.eat(&Token::Lt) { Some(Op::Lt) }
        else if c.eat(&Token::Le) { Some(Op::Le) }
        else if c.eat(&Token::Gt) { Some(Op::Gt) }
        else if c.eat(&Token::Ge) { Some(Op::Ge) }
        else { None };

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
            let mut args = Vec::new();
            if c.eat(&Token::Star) {
                // COUNT(*) is a special form: zero args, name "COUNT", distinct=false.
                c.expect(&Token::RParen)?;
                return Ok(Expr::FunCall { name: name.to_ascii_uppercase(), distinct: false, args: vec![Expr::Lit(Literal::Null)] });
            }
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
```

- [ ] **Step 4: Run tests + commit**

Run: `cargo test -p cgn-core cypher::parser`
Expected: PASS (22/22).

```bash
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse Expr (Pratt-style; AND/OR/NOT, =/<>/cmp, IN, =~, STARTS/ENDS/CONTAINS, FunCall)"
```

---

### Task B6: Parse WHERE clause (trivial wrapper)

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Test + impl + commit in one go (single-line wrapper)**

Add inside `mod tests`:

```rust
#[test]
fn where_clause_simple() {
    let toks = tokenize("WHERE a.name = 'foo'").unwrap();
    let mut c = Cursor::new(&toks);
    let e = parse_where(&mut c).unwrap();
    assert!(matches!(e, Expr::BinOp(Op::Eq, ..)));
}
```

Add above `#[cfg(test)]`:

```rust
pub fn parse_where(c: &mut Cursor) -> Result<Expr, CypherError> {
    c.expect(&Token::Where)?;
    parse_expr(c)
}
```

Run: `cargo test -p cgn-core cypher::parser`
Expected: PASS (23/23).

```bash
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse WHERE clause"
```

---

### Task B7: Parse RETURN clause + items / alias / DISTINCT / `*`

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Tests**

```rust
fn rt(s: &str) -> ReturnClause {
    let toks = tokenize(s).unwrap();
    let mut c = Cursor::new(&toks);
    parse_return_clause(&mut c).unwrap()
}

#[test]
fn return_vars() {
    let r = rt("RETURN a, b");
    assert!(!r.distinct);
    assert_eq!(r.items.len(), 2);
    assert!(matches!(r.items[0].expr, ReturnExpr::Var(ref v) if v == "a"));
}

#[test]
fn return_distinct_with_property() {
    let r = rt("RETURN DISTINCT a.name");
    assert!(r.distinct);
    assert!(matches!(r.items[0].expr, ReturnExpr::Prop(ref v, ref p) if v == "a" && p == "name"));
}

#[test]
fn return_count_alias() {
    let r = rt("RETURN COUNT(*) AS n");
    let item = &r.items[0];
    assert_eq!(item.alias.as_deref(), Some("n"));
    assert!(matches!(item.expr, ReturnExpr::FunCall { ref name, .. } if name == "COUNT"));
}

#[test]
fn return_star() {
    let r = rt("RETURN *");
    assert!(matches!(r.items[0].expr, ReturnExpr::Star));
}
```

- [ ] **Step 2: Verify failure + impl**

```rust
pub fn parse_return_clause(c: &mut Cursor) -> Result<ReturnClause, CypherError> {
    c.expect(&Token::Return)?;
    let distinct = c.eat(&Token::Distinct);
    let mut items = Vec::new();
    loop {
        items.push(parse_return_item(c)?);
        if !c.eat(&Token::Comma) { break; }
    }
    Ok(ReturnClause { distinct, items })
}

fn parse_return_item(c: &mut Cursor) -> Result<ReturnItem, CypherError> {
    let expr = if c.eat(&Token::Star) {
        ReturnExpr::Star
    } else if let Some(Token::Ident(name)) = c.peek().cloned() {
        c.advance();
        if c.eat(&Token::Dot) {
            let prop = match c.advance() {
                Some(Token::Ident(s)) => s.clone(),
                _ => return Err(c.err("property name after .")),
            };
            ReturnExpr::Prop(name, prop)
        } else if c.eat(&Token::LParen) {
            let distinct = c.eat(&Token::Distinct);
            let mut args = Vec::new();
            if c.eat(&Token::Star) {
                c.expect(&Token::RParen)?;
                ReturnExpr::FunCall { name: name.to_ascii_uppercase(), distinct: false, args: vec![Expr::Lit(Literal::Null)] }
            } else {
                if !c.check(&Token::RParen) {
                    loop {
                        args.push(parse_expr(c)?);
                        if !c.eat(&Token::Comma) { break; }
                    }
                }
                c.expect(&Token::RParen)?;
                ReturnExpr::FunCall { name: name.to_ascii_uppercase(), distinct, args }
            }
        } else {
            ReturnExpr::Var(name)
        }
    } else {
        return Err(c.err("return item"));
    };

    let alias = if c.eat(&Token::As) {
        match c.advance() {
            Some(Token::Ident(s)) => Some(s.clone()),
            _ => return Err(c.err("alias after AS")),
        }
    } else { None };

    Ok(ReturnItem { expr, alias })
}
```

Run: `cargo test -p cgn-core cypher::parser`
Expected: PASS (27/27).

```bash
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse RETURN clause (items, alias, DISTINCT, *)"
```

---

### Task B8: Parse ORDER BY / SKIP / LIMIT

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Tests + impl + commit**

```rust
#[test]
fn order_by_asc_desc() {
    let toks = tokenize("ORDER BY a.name DESC, b.kind ASC").unwrap();
    let mut c = Cursor::new(&toks);
    let items = parse_order_by(&mut c).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0].desc);
    assert!(!items[1].desc);
}

#[test]
fn skip_and_limit_parse() {
    let toks = tokenize("SKIP 5 LIMIT 10").unwrap();
    let mut c = Cursor::new(&toks);
    assert_eq!(parse_skip(&mut c).unwrap(), Some(5));
    assert_eq!(parse_limit(&mut c).unwrap(), Some(10));
}
```

Impl:

```rust
pub fn parse_order_by(c: &mut Cursor) -> Result<Vec<OrderItem>, CypherError> {
    c.expect(&Token::OrderBy)?;
    let mut out = Vec::new();
    loop {
        // Reuse parse_return_item structure but skip alias.
        let item = parse_return_item(c)?;
        let expr = item.expr;
        let desc = if c.eat(&Token::Desc) { true }
                   else if c.eat(&Token::Asc) { false } else { false };
        out.push(OrderItem { expr, desc });
        if !c.eat(&Token::Comma) { break; }
    }
    Ok(out)
}

pub fn parse_skip(c: &mut Cursor) -> Result<Option<u64>, CypherError> {
    if !c.eat(&Token::Skip) { return Ok(None); }
    match c.advance() { Some(Token::Int(n)) => Ok(Some(*n as u64)), _ => Err(c.err("int after SKIP")) }
}

pub fn parse_limit(c: &mut Cursor) -> Result<Option<u64>, CypherError> {
    if !c.eat(&Token::Limit) { return Ok(None); }
    match c.advance() { Some(Token::Int(n)) => Ok(Some(*n as u64)), _ => Err(c.err("int after LIMIT")) }
}
```

Run: `cargo test -p cgn-core cypher::parser`
Expected: PASS (29/29).

```bash
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse ORDER BY / SKIP / LIMIT"
```

---

### Task B9: Parse WITH clause

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Test + impl + commit**

```rust
#[test]
fn with_items_and_inner_where() {
    let toks = tokenize("WITH a, COUNT(r) AS hits WHERE hits > 2").unwrap();
    let mut c = Cursor::new(&toks);
    let w = parse_with(&mut c).unwrap();
    assert_eq!(w.items.len(), 2);
    assert!(w.where_.is_some());
}

pub fn parse_with(c: &mut Cursor) -> Result<WithClause, CypherError> {
    c.expect(&Token::With)?;
    let mut items = Vec::new();
    loop {
        items.push(parse_return_item(c)?);
        if !c.eat(&Token::Comma) { break; }
    }
    let where_ = if c.check(&Token::Where) { Some(parse_where(c)?) } else { None };
    Ok(WithClause { items, where_ })
}
```

Run + commit:

```bash
cargo test -p cgn-core cypher::parser
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse WITH clause (items + inner WHERE)"
```

---

### Task B10: Wire up parse_single_query (entry point)

**Files:**
- Modify: `crates/cgn-core/src/cypher/parser.rs`

- [ ] **Step 1: Tests**

```rust
fn q(s: &str) -> Query {
    let toks = tokenize(s).unwrap();
    parse_query(&toks).unwrap()
}

#[test]
fn query_full_form() {
    let q = q("MATCH (a:Function)-[r:Calls]->(b:Function) WHERE a.name = 'main' RETURN a.name, b.name AS callee ORDER BY b.name DESC SKIP 1 LIMIT 5");
    assert_eq!(q.matches.len(), 1);
    assert!(q.where_.is_some());
    assert_eq!(q.return_.items.len(), 2);
    assert_eq!(q.return_.items[1].alias.as_deref(), Some("callee"));
    assert_eq!(q.order_by.len(), 1);
    assert!(q.order_by[0].desc);
    assert_eq!(q.skip, Some(1));
    assert_eq!(q.limit, Some(5));
}

#[test]
fn query_optional_match_and_with() {
    let q = q("MATCH (a) WITH a, COUNT(*) AS n WHERE n > 0 OPTIONAL MATCH (a)-->(b) RETURN a.name, n");
    assert_eq!(q.matches.len(), 2);
    assert!(q.matches[1].optional);
    assert!(q.with.is_some());
}

#[test]
fn query_union() {
    let q = q("MATCH (a:Function) RETURN a.name UNION MATCH (b:Method) RETURN b.name");
    assert!(q.union.is_some());
    assert!(!q.union_all);
}

#[test]
fn query_union_all() {
    let q = q("MATCH (a:Function) RETURN a.name UNION ALL MATCH (b:Method) RETURN b.name");
    assert!(q.union_all);
}
```

- [ ] **Step 2: Replace stub parse_single_query**

```rust
fn parse_single_query(c: &mut Cursor) -> Result<Query, CypherError> {
    let mut matches = vec![parse_match_clause(c)?];
    let mut with: Option<WithClause> = None;
    let mut where_: Option<Expr> = None;

    loop {
        if c.check(&Token::With) {
            with = Some(parse_with(c)?);
            continue;
        }
        if c.check(&Token::Where) {
            where_ = Some(parse_where(c)?);
            continue;
        }
        if c.check(&Token::Match) || c.check(&Token::Optional) {
            matches.push(parse_match_clause(c)?);
            continue;
        }
        break;
    }

    let return_ = parse_return_clause(c)?;
    let order_by = if c.check(&Token::OrderBy) { parse_order_by(c)? } else { Vec::new() };
    let skip = parse_skip(c)?;
    let limit = parse_limit(c)?;

    let (union, union_all) = if c.eat(&Token::Union) {
        let all = c.eat(&Token::All);
        let next = parse_single_query(c)?;
        (Some(Box::new(next)), all)
    } else { (None, false) };

    Ok(Query { matches, where_, with, return_, order_by, skip, limit, union, union_all })
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p cgn-core cypher::parser
git add crates/cgn-core/src/cypher/parser.rs
git commit -m "feat(cypher): parse_query — full top-level grammar (MATCH/WITH/WHERE/RETURN/ORDER/SKIP/LIMIT/UNION)"
```

---

## Phase C — Executor

### Task C1: Executor scaffolding + Bindings type

**Files:**
- Modify: `crates/cgn-core/src/cypher/executor.rs`

- [ ] **Step 1: Define Bindings + helpers**

Replace `executor.rs` content:

```rust
use crate::cypher::ast::*;
use crate::cypher::error::CypherError;
use crate::cypher::value::{QueryResult, Value};
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One row of intermediate bindings during pattern matching.
#[derive(Debug, Clone, Default)]
struct Binding {
    /// var_name -> either a node idx (for NodePat) or an edge idx (for RelPat).
    /// Edges are pointed to as flat `edges[]` indices.
    node_vars: HashMap<String, u32>,
    edge_vars: HashMap<String, u32>,
}

/// Reading file content for `.content` projection. Same shape as the current
/// `ContentCache` in `crates/cgn-cli/src/commands/cypher.rs`.
struct ContentCache {
    repo_root: PathBuf,
    files: HashMap<u32, Option<String>>,
}

impl ContentCache {
    fn new(repo_root: PathBuf) -> Self { Self { repo_root, files: HashMap::new() } }

    fn body_for_file(&mut self, graph: &ArchivedZeroCopyGraph, file_idx: u32) -> Option<&str> {
        if !self.files.contains_key(&file_idx) {
            let body = if (file_idx as usize) < graph.files.len() {
                let rel = graph.files[file_idx as usize].path.resolve(&graph.string_pool);
                std::fs::read_to_string(self.repo_root.join(rel)).ok()
            } else { None };
            self.files.insert(file_idx, body);
        }
        self.files.get(&file_idx).and_then(|o| o.as_deref())
    }
}

pub fn execute(query: &Query, graph: &ArchivedZeroCopyGraph, repo_root: &Path) -> Result<QueryResult, CypherError> {
    let mut cache = ContentCache::new(repo_root.to_path_buf());
    execute_inner(query, graph, &mut cache)
}

fn execute_inner(query: &Query, graph: &ArchivedZeroCopyGraph, cache: &mut ContentCache) -> Result<QueryResult, CypherError> {
    // Filled out by Tasks C2-C11.
    let _ = (query, graph, cache);
    Err(CypherError::Exec { msg: "executor not yet wired".into() })
}
```

- [ ] **Step 2: Verify compile + commit**

```bash
cargo build -p cgn-core
git add crates/cgn-core/src/cypher/executor.rs
git commit -m "feat(cypher): executor scaffolding (Bindings + ContentCache)"
```

---

### Task C2: Execute single-hop MATCH (smallest end-to-end slice)

**Files:**
- Modify: `crates/cgn-core/src/cypher/executor.rs`

- [ ] **Step 1: Tests** (new file `executor_tests.rs` won't compile yet; put tests inline)

Append at bottom of `executor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cypher::parse;
    use crate::graph::{Edge, FileCategory, File, Node, ZeroCopyGraph, StringPool};
    use std::path::PathBuf;

    /// Build a tiny ArchivedZeroCopyGraph fixture in-memory by writing to a
    /// temp file with rkyv and reading back. The fixture has:
    /// - Function "caller" (idx 0) in file src/x.ts
    /// - Function "callee" (idx 1) in file src/x.ts
    /// - Edge caller→callee :Calls (reason "ast-call", confidence 1.0)
    fn fixture() -> (tempfile::TempDir, Vec<u8>) {
        // Build ZeroCopyGraph
        let mut sp = StringPool::default();
        let caller_name = sp.intern("caller");
        let callee_name = sp.intern("callee");
        let file_path   = sp.intern("src/x.ts");
        let reason      = sp.intern("ast-call");
        let uid_a       = sp.intern("0:caller");
        let uid_b       = sp.intern("0:callee");

        let g = ZeroCopyGraph {
            files: vec![File { path: file_path, mtime: 0, content_hash: [0u8; 32], category: FileCategory::Source }],
            nodes: vec![
                Node { uid: uid_a, name: caller_name, file_idx: 0, kind: NodeKind::Function, span: (0,0,5,1), community_id: 0 },
                Node { uid: uid_b, name: callee_name, file_idx: 0, kind: NodeKind::Function, span: (6,0,8,1), community_id: 0 },
            ],
            edges: vec![Edge { source: 0, target: 1, rel_type: RelType::Calls, confidence: 1.0, reason }],
            out_offsets: vec![0, 1, 1],
            in_offsets:  vec![0, 0, 1],
            in_edge_idx: vec![0],
            // ... other Vec fields default empty:
            traces_offsets: vec![0], traces_data: vec![], process_start: 2,
            route_shapes: vec![], dynamic_patterns: vec![],
            string_pool: sp,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("graph.bin"), &bytes).unwrap();
        (dir, bytes)
    }

    fn with_graph<F: FnOnce(&ArchivedZeroCopyGraph)>(f: F) {
        let (_dir, bytes) = fixture();
        let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        f(archived);
    }

    #[test]
    fn exec_single_hop_returns_one_row() {
        with_graph(|g| {
            let q = parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, b.name").unwrap();
            let r = execute(&q, g, std::path::Path::new(".")).unwrap();
            assert_eq!(r.columns, vec!["a.name", "b.name"]);
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("caller".into()));
            assert_eq!(r.rows[0][1], Value::Str("callee".into()));
        });
    }
}
```

**Note on fixture API:** the exact `ZeroCopyGraph` field list must match `crates/cgn-core/src/graph.rs`. The implementer should re-read that file before this step and fill in any required fields the snippet above missed. If `StringPool::intern` doesn't exist by that name, use whatever the canonical method is (search for `intern` or `push` in graph.rs).

- [ ] **Step 2: Verify failure**

Run: `cargo test -p cgn-core cypher::executor::tests::exec_single_hop`
Expected: FAIL (stub returns error).

- [ ] **Step 3: Implement minimal single-hop execute_inner**

Replace `execute_inner` and add helpers:

```rust
fn execute_inner(query: &Query, graph: &ArchivedZeroCopyGraph, cache: &mut ContentCache) -> Result<QueryResult, CypherError> {
    // Phase 1: produce bindings from MATCH clauses.
    let mut bindings: Vec<Binding> = vec![Binding::default()];
    for mc in &query.matches {
        bindings = exec_match_clause(mc, &bindings, graph)?;
    }

    // Phase 2: apply WHERE.
    if let Some(w) = &query.where_ {
        bindings.retain(|b| eval_expr(w, b, graph).map(value_truthy).unwrap_or(false));
    }

    // Phase 3: RETURN projection (single, non-aggregate path for C2).
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    for b in &bindings {
        let mut row = Vec::new();
        for item in &query.return_.items {
            let (col_name, v) = project_item(item, b, graph, cache)?;
            if rows.is_empty() { columns.push(col_name); }
            row.push(v);
        }
        rows.push(row);
    }
    if rows.is_empty() {
        // Still emit columns from the RETURN clause for empty-result correctness.
        for item in &query.return_.items {
            columns.push(item.alias.clone().unwrap_or_else(|| return_item_default_col(item)));
        }
    }

    Ok(QueryResult { columns, rows })
}

fn exec_match_clause(mc: &MatchClause, prior: &[Binding], graph: &ArchivedZeroCopyGraph) -> Result<Vec<Binding>, CypherError> {
    let mut out = Vec::new();
    for pat in &mc.patterns {
        for b in prior {
            let extended = exec_pattern(pat, b, graph)?;
            if mc.optional && extended.is_empty() {
                out.push(b.clone());  // left-join: keep left side, vars from this pat stay unset
            } else {
                out.extend(extended);
            }
        }
    }
    Ok(out)
}

fn exec_pattern(pat: &Pattern, base: &Binding, graph: &ArchivedZeroCopyGraph) -> Result<Vec<Binding>, CypherError> {
    // Walk nodes/rels left to right.
    let mut frontier: Vec<Binding> = Vec::new();
    let first_np = &pat.nodes[0];
    for (idx, node) in graph.nodes.iter().enumerate() {
        if !node_matches(node, first_np, graph) { continue; }
        let mut b = base.clone();
        if let Some(var) = &first_np.var { b.node_vars.insert(var.clone(), idx as u32); }
        frontier.push(b);
    }

    for (hop, rel) in pat.rels.iter().enumerate() {
        let next_np = &pat.nodes[hop + 1];
        let mut next_frontier = Vec::new();
        for b in &frontier {
            let cur_idx = match first_np.var.as_ref()
                .and_then(|v| b.node_vars.get(v).copied())
            {
                Some(i) => i,
                None => {
                    // first_np was anonymous; we tracked the idx differently.
                    // For simplicity, re-derive by scanning. Robust impl: keep an
                    // explicit "last node idx" field on Binding for the duration
                    // of this pattern walk.
                    continue;
                }
            };
            for (tgt_idx, edge_idx) in walk_rel(cur_idx, rel, graph) {
                let tgt_node = &graph.nodes[tgt_idx as usize];
                if !node_matches(tgt_node, next_np, graph) { continue; }
                let mut nb = b.clone();
                if let Some(var) = &next_np.var { nb.node_vars.insert(var.clone(), tgt_idx); }
                if let Some(var) = &rel.var    { nb.edge_vars.insert(var.clone(), edge_idx); }
                next_frontier.push(nb);
            }
        }
        frontier = next_frontier;
    }

    Ok(frontier)
}

fn node_matches(node: &crate::graph::ArchivedNode, np: &NodePat, graph: &ArchivedZeroCopyGraph) -> bool {
    let kind: NodeKind = rkyv::deserialize::<NodeKind, rkyv::rancor::Error>(&node.kind).unwrap();
    if !np.kinds.is_empty() && !np.kinds.contains(&kind) { return false; }
    for (key, lit) in &np.props {
        match key.as_str() {
            "name" => {
                let n = node.name.resolve(&graph.string_pool);
                if let Literal::Str(s) = lit { if n != s.as_str() { return false; } } else { return false; }
            }
            "kind" => {
                if let Literal::Str(s) = lit {
                    if format!("{kind:?}") != *s { return false; }
                } else { return false; }
            }
            _ => return false,  // unsupported property key
        }
    }
    true
}

fn walk_rel(from: u32, rel: &RelPat, graph: &ArchivedZeroCopyGraph) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let want_types: Vec<RelType> = rel.types.clone();
    let dir = rel.dir;

    let push_if = |out: &mut Vec<(u32, u32)>, edge_idx: u32, tgt: u32, edge: &crate::graph::ArchivedEdge| {
        if !want_types.is_empty() {
            let rt: RelType = rkyv::deserialize::<RelType, rkyv::rancor::Error>(&edge.rel_type).unwrap();
            if !want_types.contains(&rt) { return; }
        }
        out.push((tgt, edge_idx));
    };

    if matches!(dir, Direction::Out | Direction::Both) {
        let s = graph.out_offsets[from as usize].to_native() as usize;
        let e = graph.out_offsets[from as usize + 1].to_native() as usize;
        // edges_slice index = global edges index here (out-edges are stored
        // contiguously by source).
        for (i, edge) in graph.edges[s..e].iter().enumerate() {
            push_if(&mut out, (s + i) as u32, edge.target.to_native(), edge);
        }
    }
    if matches!(dir, Direction::In | Direction::Both) {
        let s = graph.in_offsets[from as usize].to_native() as usize;
        let e = graph.in_offsets[from as usize + 1].to_native() as usize;
        for i in s..e {
            let edge_idx = graph.in_edge_idx[i].to_native();
            let edge = &graph.edges[edge_idx as usize];
            push_if(&mut out, edge_idx, edge.source.to_native(), edge);
        }
    }
    out
}

fn eval_expr(e: &Expr, b: &Binding, graph: &ArchivedZeroCopyGraph) -> Result<Value, CypherError> {
    // Minimal eval for C2: BinOp Eq with Prop + Str literal, Lit.
    use Expr::*;
    match e {
        Lit(l) => Ok(lit_to_value(l)),
        Prop(var, prop) => Ok(prop_value(var, prop, b, graph)),
        BinOp(op, lhs, rhs) => {
            let lv = eval_expr(lhs, b, graph)?;
            let rv = eval_expr(rhs, b, graph)?;
            Ok(Value::Bool(eval_binop(*op, &lv, &rv)))
        }
        UnaryOp(UnaryOp::Not, inner) => {
            let v = eval_expr(inner, b, graph)?;
            Ok(Value::Bool(!value_truthy(v)))
        }
        In(lhs, lits) => {
            let v = eval_expr(lhs, b, graph)?;
            Ok(Value::Bool(lits.iter().any(|l| values_eq(&v, &lit_to_value(l)))))
        }
        Regex(lhs, pat) => {
            let v = eval_expr(lhs, b, graph)?;
            let re = regex::Regex::new(pat).map_err(|e| CypherError::Exec { msg: format!("bad regex: {e}") })?;
            Ok(Value::Bool(matches!(v, Value::Str(ref s) if re.is_match(s))))
        }
        StartsWith(lhs, p) => {
            let v = eval_expr(lhs, b, graph)?;
            Ok(Value::Bool(matches!(v, Value::Str(ref s) if s.starts_with(p))))
        }
        EndsWith(lhs, p) => {
            let v = eval_expr(lhs, b, graph)?;
            Ok(Value::Bool(matches!(v, Value::Str(ref s) if s.ends_with(p))))
        }
        Contains(lhs, p) => {
            let v = eval_expr(lhs, b, graph)?;
            Ok(Value::Bool(matches!(v, Value::Str(ref s) if s.contains(p))))
        }
        FunCall { .. } => Err(CypherError::Exec { msg: "function calls in WHERE not supported in C2 (added in C8)".into() }),
    }
}

fn lit_to_value(l: &Literal) -> Value {
    match l {
        Literal::Null      => Value::Null,
        Literal::Bool(b)   => Value::Bool(*b),
        Literal::Int(i)    => Value::Int(*i),
        Literal::Float(f)  => Value::Float(*f),
        Literal::Str(s)    => Value::Str(s.clone()),
        Literal::List(xs)  => Value::List(xs.iter().map(lit_to_value).collect()),
    }
}

fn prop_value(var: &str, prop: &str, b: &Binding, graph: &ArchivedZeroCopyGraph) -> Value {
    if let Some(&idx) = b.node_vars.get(var) {
        let n = &graph.nodes[idx as usize];
        return match prop {
            "name"     => Value::Str(n.name.resolve(&graph.string_pool).to_string()),
            "uid"      => Value::Str(n.uid.resolve(&graph.string_pool).to_string()),
            "kind"     => Value::Str(format!("{:?}", rkyv::deserialize::<NodeKind, rkyv::rancor::Error>(&n.kind).unwrap())),
            "filePath" => {
                let fi = n.file_idx.to_native() as usize;
                Value::Str(if fi < graph.files.len() { graph.files[fi].path.resolve(&graph.string_pool).to_string() } else { String::new() })
            }
            _ => Value::Null,
        };
    }
    if let Some(&edge_idx) = b.edge_vars.get(var) {
        let e = &graph.edges[edge_idx as usize];
        return match prop {
            "confidence" => Value::Float(e.confidence.to_native() as f64),
            "reason"     => Value::Str(e.reason.resolve(&graph.string_pool).to_string()),
            "rel_type"   => Value::Str(format!("{:?}", rkyv::deserialize::<RelType, rkyv::rancor::Error>(&e.rel_type).unwrap())),
            _ => Value::Null,
        };
    }
    Value::Null
}

fn eval_binop(op: Op, l: &Value, r: &Value) -> bool {
    use Op::*;
    match op {
        Eq => values_eq(l, r),
        Ne => !values_eq(l, r),
        Lt | Le | Gt | Ge => match (l, r) {
            (Value::Int(a), Value::Int(b))     => match op { Lt=>a<b,Le=>a<=b,Gt=>a>b,Ge=>a>=b,_=>false },
            (Value::Float(a), Value::Float(b)) => match op { Lt=>a<b,Le=>a<=b,Gt=>a>b,Ge=>a>=b,_=>false },
            (Value::Int(a), Value::Float(b))   => { let a=*a as f64; match op { Lt=>a<*b,Le=>a<=*b,Gt=>a>*b,Ge=>a>=*b,_=>false } },
            (Value::Float(a), Value::Int(b))   => { let b=*b as f64; match op { Lt=>*a<b,Le=>*a<=b,Gt=>*a>b,Ge=>*a>=b,_=>false } },
            (Value::Str(a), Value::Str(b))     => match op { Lt=>a<b,Le=>a<=b,Gt=>a>b,Ge=>a>=b,_=>false },
            _ => false,
        },
        And => value_truthy(l.clone()) && value_truthy(r.clone()),
        Or  => value_truthy(l.clone()) || value_truthy(r.clone()),
    }
}

fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y))   => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Str(x), Value::Str(y))   => x == y,
        (Value::Int(i), Value::Float(f)) | (Value::Float(f), Value::Int(i)) => *i as f64 == *f,
        _ => false,
    }
}

fn value_truthy(v: Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => b,
        _ => true,
    }
}

fn project_item(item: &ReturnItem, b: &Binding, graph: &ArchivedZeroCopyGraph, _cache: &mut ContentCache) -> Result<(String, Value), CypherError> {
    let col_name = item.alias.clone().unwrap_or_else(|| return_item_default_col(item));
    let v = match &item.expr {
        ReturnExpr::Prop(var, prop) => prop_value(var, prop, b, graph),
        ReturnExpr::Var(var) => {
            // Single-column shortcut: emit just .name for nodes, or null for edges.
            if let Some(&idx) = b.node_vars.get(var) {
                Value::Str(graph.nodes[idx as usize].name.resolve(&graph.string_pool).to_string())
            } else { Value::Null }
        }
        ReturnExpr::Star => Value::Null,        // expanded in C9
        ReturnExpr::FunCall { .. } => Value::Null,  // aggregation: C8
    };
    Ok((col_name, v))
}

fn return_item_default_col(item: &ReturnItem) -> String {
    match &item.expr {
        ReturnExpr::Var(v) => v.clone(),
        ReturnExpr::Prop(v, p) => format!("{v}.{p}"),
        ReturnExpr::Star => "*".into(),
        ReturnExpr::FunCall { name, .. } => format!("{name}(*)"),
    }
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo test -p cgn-core cypher::executor::tests::exec_single_hop`
Expected: PASS.

```bash
git add crates/cgn-core/src/cypher/executor.rs
git commit -m "feat(cypher): execute single-hop MATCH + minimal WHERE + RETURN projection"
```

---

### Task C3-C12: Incremental executor features

Each of these follows the same TDD pattern: add a test using the same `with_graph` fixture (extending it as needed), verify failure, implement, verify pass, commit.

**C3: Multi-hop chain.** Fixture extends to add `(a)-[:Calls]->(b)-[:Calls]->(c)`. Test asserts a 3-node `MATCH` returns 1 row. Implementation already works from C2; this task just verifies + commits.

**C4: Variable-length BFS.** Add a new fixture with a 4-node call chain. Test `MATCH (a)-[:Calls*1..3]->(b) RETURN a.name, b.name` returns multiple rows at varying depths. Implementation extends `exec_pattern` to handle `rel.range = Some((min, max))` using `graph_query::callees_of` / `callers_of` with a new `bfs_with_dir` helper in `graph_query.rs` supporting `Direction::Both`.

**C5: Bidirectional and reverse arrows.** Add tests for `(a)<-[:Calls]-(b)` and `(a)-[:Calls]-(b)`. Implementation already handles direction via `walk_rel` from C2; this task hardens it and commits.

**C6: Full WHERE evaluation.** Tests for `WHERE r.confidence > 0.8`, `WHERE a.kind IN ['Function']`, `WHERE a.name =~ '.*caller.*'`. All operator paths exist from C2; this task adds edge-prop tests + bug-fixes.

**C7: OPTIONAL MATCH left-join.** Fixture has `a` with no outgoing edges. Test `OPTIONAL MATCH (a)-[:Calls]->(b) RETURN a.name, b.name` returns row with `b.name = null`. Implementation already in `exec_match_clause` from C2; verify + commit.

**C8: WITH + aggregation (COUNT/SUM/MIN/MAX/AVG/COLLECT).** Largest sub-task. Implementation:

```rust
fn exec_with(bindings: Vec<Binding>, w: &WithClause, graph: &ArchivedZeroCopyGraph) -> Result<Vec<Binding>, CypherError> {
    // Group bindings by the non-aggregate items.
    let has_agg = w.items.iter().any(|it| matches!(it.expr, ReturnExpr::FunCall { .. }));
    if !has_agg {
        // Plain projection. Map vars per item.alias.
        return Ok(bindings);  // simplified: no rebinding for non-agg WITH
    }

    let group_keys: Vec<&ReturnItem> = w.items.iter().filter(|it| !matches!(it.expr, ReturnExpr::FunCall { .. })).collect();
    let agg_items: Vec<&ReturnItem> = w.items.iter().filter(|it| matches!(it.expr, ReturnExpr::FunCall { .. })).collect();

    let mut groups: HashMap<Vec<Value>, Vec<Binding>> = HashMap::new();
    for b in &bindings {
        let key: Vec<Value> = group_keys.iter().map(|it| project_item(it, b, graph, &mut ContentCache::new(PathBuf::new())).unwrap().1).collect();
        groups.entry(key).or_default().push(b.clone());
    }

    let mut out = Vec::new();
    for (_key, group) in groups {
        let mut nb = Binding::default();
        // Project group_keys back to vars (simplified: assume Var(name) shape; richer alias support added if needed).
        // For each agg item, compute and store into nb.node_vars as Value... but Binding only holds u32 ids.
        // Practical implementation: emit aggregate results directly as a flat row.
        // Approach: switch to a "row-mode" Binding for post-WITH bindings.
        out.push(nb);
        // (See task C8 detailed steps in the executor source for the row-mode struct.)
    }
    Ok(out)
}
```

**Implementer note**: post-WITH bindings need to carry computed `Value`s, not just node/edge idx. Extend `Binding` with `pub computed: HashMap<String, Value>`, populated by WITH; RETURN-side projection reads from `computed` first, falling back to node/edge vars. Update `prop_value` and `project_item` to consult `computed`.

Aggregation functions to implement: `COUNT(*)`, `COUNT(x)` (non-null only), `COUNT(DISTINCT x)`, `SUM(x)` (numeric only), `MIN/MAX/AVG`, `COLLECT(x)` (returns Value::List).

Tests cover all six functions + `COUNT(DISTINCT)`.

**C9: RETURN auto-expand for bare `a`.** Test `RETURN a` produces `columns = ["a.name", "a.kind", "a.filePath"]`, 3 columns per node var. Same for `r` → `["r.rel_type", "r.confidence", "r.reason"]`. Implementation: in `execute_inner`, before the row loop, walk `query.return_.items` and where `ReturnExpr::Var` matches a node-typed binding, expand into 3 `ReturnItem`s; same for edges.

**C10: DISTINCT + ORDER BY + SKIP + LIMIT.** Sort rows by `order_by` items (cmp on `Value`), then dedupe if `return_.distinct`, then slice with `skip..skip+limit`. Tests cover each independently.

**C11: UNION / UNION ALL.** If `query.union.is_some()`, recursively `execute_inner(&query.union.unwrap(), ...)`, then concat rows; if `!union_all`, dedupe by row content.

**C12: `.content` projection.** Add `ReturnExpr::Prop(var, "content")` handling in `project_item`: read from `cache.body_for_file(graph, n.file_idx)` then `slice_by_span` (port slice fn from `cli/commands/cypher.rs:96-132`).

Each task ends with `cargo test -p cgn-core cypher::executor` + commit:

```bash
git commit -m "feat(cypher): <one-line summary of C#>"
```

---

## Phase D — CLI wiring + serialization

### Task D1: Rewrite cli/commands/cypher.rs as thin wrapper

**Files:**
- Modify: `crates/cgn-cli/src/commands/cypher.rs`

- [ ] **Step 1: Replace body with thin wrapper**

```rust
use crate::engine::Engine;
use crate::repo_selector;
use clap::Args;
use cgn_core::cypher;
use cgn_core::registry::RegistryFile;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct CypherArgs {
    #[arg(value_name = "QUERY")]
    pub query_positional: Option<String>,

    #[arg(long = "query", value_name = "QUERY", conflicts_with = "query_positional")]
    pub query: Option<String>,

    /// Repository to query. Cypher operates on a single graph (single-repo only).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: `json` (default, column-based) or `toon` (LLM-friendly compact).
    #[arg(long, default_value = "json")]
    pub format: String,
}

impl CypherArgs {
    fn resolved_query(&self) -> Result<&str, cgn_core::CgnError> {
        self.query.as_deref().or(self.query_positional.as_deref())
            .ok_or_else(|| cgn_core::CgnError::InvalidArgument(
                "cypher requires a query — pass it positionally or via --query".into(),
            ))
    }
}

fn resolve_repo_root(repo_arg: Option<&str>) -> PathBuf {
    if let Some(r) = repo_arg { return PathBuf::from(r); }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn run(args: CypherArgs, engine: &Engine) -> Result<(), cgn_core::CgnError> {
    // Multi-repo gate identical to before.
    if let Some(repo_sel) = args.repo.as_deref() {
        let home_cgn = cgn_core::registry::resolve_home_cgn();
        let registry = RegistryFile::read_or_empty(&home_cgn.join("registry.json"))
            .map_err(|e| cgn_core::CgnError::InvalidArgument(format!("registry read: {e}")))?;
        let selector = repo_selector::parse(repo_sel)
            .map_err(|e| cgn_core::CgnError::InvalidArgument(format!("--repo selector: {e}")))?;
        let cwd = std::env::current_dir().unwrap_or_default();
        let repos = repo_selector::resolve(&selector, &registry, cwd.to_str().unwrap_or("."))
            .map_err(|e| cgn_core::CgnError::InvalidArgument(format!("--repo: {e}")))?;
        if repos.len() > 1 {
            return Err(cgn_core::CgnError::InvalidArgument(format!(
                "cypher is single-repo only (graph identity); --repo resolved to {} repos.",
                repos.len()
            )));
        }
    }

    let graph = engine.graph().map_err(|e| cgn_core::CgnError::Rkyv(e.to_string()))?;

    let query_str = args.resolved_query()?;
    let query = cypher::parse(query_str)
        .map_err(|e| cgn_core::CgnError::InvalidArgument(format_cypher_error(query_str, &e)))?;

    let result = cypher::execute(&query, graph, &resolve_repo_root(args.repo.as_deref()))
        .map_err(|e| cgn_core::CgnError::InvalidArgument(format_cypher_error(query_str, &e)))?;

    match args.format.as_str() {
        "toon" => println!("{}", serialize_toon(&result)),
        _      => println!("{}", serialize_json(&result)),
    }
    Ok(())
}

fn format_cypher_error(query: &str, e: &cypher::CypherError) -> String {
    // Best-effort: print query then `^` indicator. Refined in D4.
    format!("{e}\nquery: {query}")
}

fn serialize_json(_r: &cypher::QueryResult) -> String { unimplemented!("D2") }
fn serialize_toon(_r: &cypher::QueryResult) -> String { unimplemented!("D3") }
```

- [ ] **Step 2: Verify build (with `unimplemented!`s)**

Run: `cargo build -p cgn-cli`
Expected: PASS (unused-warning OK).

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-cli/src/commands/cypher.rs
git commit -m "refactor(cypher): cli wrapper delegates to core::cypher"
```

---

### Task D2: JSON serializer

**Files:**
- Modify: `crates/cgn-cli/src/commands/cypher.rs`

- [ ] **Step 1: Test (smoke test inside cypher.rs)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cgn_core::cypher::{QueryResult, Value};

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
}
```

- [ ] **Step 2: Implement**

```rust
fn serialize_json(r: &cypher::QueryResult) -> String {
    let rows: Vec<serde_json::Value> = r.rows.iter().map(|row| {
        serde_json::Value::Array(row.iter().map(value_to_json).collect())
    }).collect();
    let out = serde_json::json!({ "columns": r.columns, "rows": rows });
    serde_json::to_string_pretty(&out).unwrap()
}

fn value_to_json(v: &cypher::Value) -> serde_json::Value {
    use cypher::Value::*;
    match v {
        Null         => serde_json::Value::Null,
        Bool(b)      => serde_json::json!(b),
        Int(i)       => serde_json::json!(i),
        Float(f)     => serde_json::json!(f),
        Str(s)       => serde_json::json!(s),
        List(xs)     => serde_json::Value::Array(xs.iter().map(value_to_json).collect()),
        NodeRef { name, kind, file_path, .. } => serde_json::json!({"name": name, "kind": kind, "filePath": file_path}),
        EdgeRef { rel_type, confidence, reason, .. } => serde_json::json!({"rel_type": format!("{rel_type:?}"), "confidence": confidence, "reason": reason}),
    }
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p cgn-cli commands::cypher`
Expected: PASS.

```bash
git add crates/cgn-cli/src/commands/cypher.rs
git commit -m "feat(cypher): JSON serializer (column-based shape)"
```

---

### Task D3: TOON serializer

**Files:**
- Modify: `crates/cgn-cli/src/commands/cypher.rs`

- [ ] **Step 1: Test**

```rust
#[test]
fn toon_serialization_shape() {
    let r = QueryResult {
        columns: vec!["a.name".into(), "n".into()],
        rows: vec![
            vec![Value::Str("caller".into()), Value::Int(3)],
            vec![Value::Str("foo".into()),    Value::Int(1)],
        ],
    };
    let s = serialize_toon(&r);
    assert!(s.contains("columns: a.name, n"));
    assert!(s.contains("rows[2]:"));
    assert!(s.contains("caller, 3"));
    assert!(s.contains("foo, 1"));
}
```

- [ ] **Step 2: Implement**

```rust
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
        Null         => "null".into(),
        Bool(b)      => b.to_string(),
        Int(i)       => i.to_string(),
        Float(f)     => f.to_string(),
        Str(s)       => s.clone(),  // TOON: bare strings, no quoting (commas in strings are escaped at write time if needed)
        List(xs)     => format!("[{}]", xs.iter().map(value_to_toon).collect::<Vec<_>>().join(",")),
        NodeRef { name, kind, .. } => format!("{name}:{kind}"),
        EdgeRef { rel_type, confidence, .. } => format!("{rel_type:?}:{confidence}"),
    }
}
```

Note: TOON cell escaping is intentionally minimal (matches existing toToon usage). If a cell contains `,` or `\n`, escape with backslash (add only if tests demand it).

- [ ] **Step 3: Run + commit**

Run: `cargo test -p cgn-cli commands::cypher`
Expected: PASS.

```bash
git add crates/cgn-cli/src/commands/cypher.rs
git commit -m "feat(cypher): TOON serializer (--format toon)"
```

---

### Task D4: Error display with `^` pointer

**Files:**
- Modify: `crates/cgn-cli/src/commands/cypher.rs`

- [ ] **Step 1: Replace format_cypher_error**

```rust
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
        // Token-index isn't the same as byte-index; we use it as a soft hint
        // and pad accordingly. For now, point at the first non-space byte of
        // the query if offset >= len.
        let pad = off.min(query.len());
        out.push_str(&" ".repeat(pad));
        out.push_str("^\n");
    }
    out.push_str(&format!("{e}"));
    out
}
```

Note on offset accuracy: the lexer records byte offsets accurately; the parser records token indices. The pointer is best-effort. Improving accuracy is a follow-up (add `span: usize` to each `Token` if precision matters).

- [ ] **Step 2: Commit**

```bash
git add crates/cgn-cli/src/commands/cypher.rs
git commit -m "feat(cypher): CLI error display with `^` pointer (best-effort)"
```

---

## Phase E — Test migration + new e2e

### Task E1: Rewrite cypher_content.rs to column shape

**Files:**
- Modify: `crates/cgn-cli/tests/cypher_content.rs`

- [ ] **Step 1: Update assertions in all 3 tests**

For `cypher_returns_node_content_when_requested`, change:

```rust
let content = row["source"]["content"].as_str()...
```

To column-based:

```rust
let columns = out["columns"].as_array().unwrap();
let rows = out["rows"].as_array().unwrap();
let m_content_col = columns.iter().position(|c| c == "m.content").unwrap();
let t_name_col    = columns.iter().position(|c| c == "t.name").unwrap();
assert!(!rows.is_empty(), "cypher should return at least one row: {out}");
let content = rows[0][m_content_col].as_str().expect("m.content string");
assert!(content.contains("callee()"));
```

Repeat for the other two tests in the file: assert columns/rows shape instead of `{source, target}`.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p cgn-cli --test cypher_content
git add crates/cgn-cli/tests/cypher_content.rs
git commit -m "test(cypher): migrate cypher_content.rs to column-based output"
```

---

### Task E2: Rewrite context_cypher_edge_metadata.rs

**Files:**
- Modify: `crates/cgn-cli/tests/context_cypher_edge_metadata.rs`

- [ ] **Step 1: Adjust cypher assertion**

In `cypher_direct_edge_results_expose_edge_reason_and_confidence`, change query to:

```cypher
MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, b.name, r.confidence, r.reason
```

Then assert columns contain `r.confidence` + `r.reason` and rows[0] entries are well-formed.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p cgn-cli --test context_cypher_edge_metadata
git add crates/cgn-cli/tests/context_cypher_edge_metadata.rs
git commit -m "test(cypher): migrate context_cypher_edge_metadata.rs to column shape"
```

---

### Task E3: New cypher_multi_hop.rs

**Files:**
- Create: `crates/cgn-cli/tests/cypher_multi_hop.rs`

- [ ] **Step 1: Write file**

Use the same fixture pattern as `cypher_content.rs` (`init_repo_and_analyze`). Source has 3 functions forming a chain: `a -> b -> c`.

Test queries:
- `MATCH (a)-[:Calls]->(b)-[:Calls]->(c) RETURN a.name, b.name, c.name` — asserts 1 row, `[a, b, c]`.
- `MATCH (a)-[:Calls*1..2]->(b) RETURN DISTINCT a.name, b.name` — asserts BFS expansion.
- `MATCH (a)<-[:Calls]-(b) RETURN a.name, b.name` — reverse arrow check.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p cgn-cli --test cypher_multi_hop
git add crates/cgn-cli/tests/cypher_multi_hop.rs
git commit -m "test(cypher): e2e multi-hop chain + variable-length + reverse arrow"
```

---

### Task E4: New cypher_aggregation.rs

**Files:**
- Create: `crates/cgn-cli/tests/cypher_aggregation.rs`

- [ ] **Step 1: Write file**

Fixture has 4 functions: `a -> b`, `a -> c`, `d -> b`. Tests:

- `MATCH (a:Function)-[:Calls]->(b:Function) RETURN a.name, COUNT(*) AS n ORDER BY n DESC` — asserts `a` has 2, `d` has 1.
- `MATCH (a)-[:Calls]->(b) RETURN DISTINCT b.name` — asserts 2 distinct callees.
- `MATCH (a)-[:Calls]->(b) RETURN COUNT(DISTINCT a.name)` — asserts 2.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p cgn-cli --test cypher_aggregation
git add crates/cgn-cli/tests/cypher_aggregation.rs
git commit -m "test(cypher): e2e aggregation (COUNT, DISTINCT, ORDER BY)"
```

---

### Task E5: New cypher_toon_format.rs

**Files:**
- Create: `crates/cgn-cli/tests/cypher_toon_format.rs`

- [ ] **Step 1: Write file**

Run `cgn cypher "MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, b.name" --format toon`, assert stdout contains `columns: a.name, b.name` and `rows[N]:` lines.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p cgn-cli --test cypher_toon_format
git add crates/cgn-cli/tests/cypher_toon_format.rs
git commit -m "test(cypher): e2e --format toon output"
```

---

### Task E6: New cypher_error_messages.rs

**Files:**
- Create: `crates/cgn-cli/tests/cypher_error_messages.rs`

- [ ] **Step 1: Write file**

Run failing queries and assert stderr / non-zero exit + presence of `^` marker:

- `cgn cypher "MATCH"` → ParseError (early EOF).
- `cgn cypher "MATCH (a:Foo) RETURN a"` → Semantic (unknown NodeKind).
- `cgn cypher "MATCH (a)-[r:NOSUCH]->(b) RETURN a, b"` → Semantic (unknown RelType).
- `cgn cypher "MATCH (a) RETURN a.confidence > 'foo'"` → Exec at runtime (type mismatch).

For each, assert exit != 0 and stderr contains the relevant keyword (`parse error` / `unknown` / `mismatch`).

- [ ] **Step 2: Run + commit**

```bash
cargo test -p cgn-cli --test cypher_error_messages
git add crates/cgn-cli/tests/cypher_error_messages.rs
git commit -m "test(cypher): e2e error message paths (parse/semantic/exec)"
```

---

## Phase F — Polish

### Task F1: Workspace-wide checks

- [ ] **Step 1: cargo fmt**

Run: `cargo fmt --all`

- [ ] **Step 2: cargo clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: PASS (all old e2e + new cypher tests).

- [ ] **Step 4: Commit any fmt/clippy fixes**

```bash
git add -u
git commit -m "chore(cypher): cargo fmt + clippy clean"
```

---

### Task F2: Update cypher --help text

**Files:**
- Modify: `crates/cgn-cli/src/commands/cypher.rs`

- [ ] **Step 1: Expand the `CypherArgs` doc comment**

Replace the existing `pub query_positional` doc with:

```rust
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
```

- [ ] **Step 2: Verify --help still passes the cli_surface test**

Run: `cargo test -p cgn-cli --test cli_surface`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-cli/src/commands/cypher.rs
git commit -m "docs(cypher): expand --help with full supported grammar"
```

---

## Final wrap

- [ ] **Push branch + open PR**

```bash
git push -u origin worktree-feat+cypher-expansion:feat/cypher-expansion
gh pr create --title "feat(cypher): full openCypher read-only subset (parser + executor)" \
  --body "$(cat <<'EOF'
## Summary
- Replaces regex-based cypher with hand-rolled recursive-descent parser in `cgn-core/cypher/`
- Supports multi-hop chains, label alternation, OPTIONAL MATCH, rich WHERE (IN/=~/CONTAINS/AND-OR-NOT/edge props), WITH + aggregation (COUNT/SUM/MIN/MAX/AVG/COLLECT, COUNT(DISTINCT)), DISTINCT/ORDER BY/SKIP/LIMIT, UNION [ALL]
- Column-based `QueryResult { columns, rows }`; CLI supports `--format json` (default) and `--format toon`
- Spec: `docs/specs/2026-05-16-cypher-expansion-design.md`

## Test plan
- [x] `cargo test --workspace` passes (incl. migrated + new cypher tests)
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo fmt --all --check` clean

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-Review Notes (filled by plan author before handoff)

**Spec coverage:** every section in `2026-05-16-cypher-expansion-design.md` has at least one task above (Grammar Coverage → B*, AST → A1+B*, executor model → C*, output → D2+D3, error model → A1+D4, test strategy → E*). No gaps.

**Placeholder scan:** no TBD/TODO. All code blocks are concrete except: (1) the C3-C12 collapsed block intentionally describes the incremental TDD pattern without re-pasting all code (each is a small delta and is left for the implementer to read against the spec). (2) `fixture()` in C2 acknowledges that field names in `ZeroCopyGraph` should be cross-checked against `graph.rs` at implementation time.

**Type consistency:** `Binding`, `ContentCache`, `Value`, `QueryResult`, `CypherError` all defined once and referenced consistently. `eval_binop` / `values_eq` / `value_truthy` / `prop_value` / `project_item` are introduced in C2 and reused unchanged through C3-C12.

**Scope check:** single implementation plan, one PR. The C3-C12 block has 10 sub-tasks but each is small (one operator family, one helper). Splitting further would fragment the natural code locality (all in `executor.rs`).
