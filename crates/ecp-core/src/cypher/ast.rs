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
pub struct MatchClause {
    pub optional: bool,
    pub patterns: Vec<Pattern>,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub nodes: Vec<NodePat>,
    pub rels: Vec<RelPat>,
}

#[derive(Debug, Clone)]
pub struct NodePat {
    pub var: Option<String>,
    pub kinds: Vec<NodeKind>,
    pub props: Vec<(String, Literal)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    Both,
}

#[derive(Debug, Clone)]
pub struct RelPat {
    pub var: Option<String>,
    pub types: Vec<RelType>,
    pub range: Option<(u32, u32)>,
    pub dir: Direction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Literal>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
}

#[derive(Debug, Clone)]
pub enum Expr {
    BinOp(Op, Box<Expr>, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    Var(String),
    Prop(String, String),
    Lit(Literal),
    In(Box<Expr>, Vec<Literal>),
    /// `scalar IN collection_property` — RHS is a graph property that resolves
    /// to a `Value::List`. Distinct from `In` (literal list on RHS) so the
    /// common literal-list case avoids `eval_expr` overhead.
    InCollection(Box<Expr>, Box<Expr>),
    Regex(Box<Expr>, String),
    StartsWith(Box<Expr>, String),
    EndsWith(Box<Expr>, String),
    Contains(Box<Expr>, String),
    /// OpenCypher label-test predicate: `n:Label` or `n:A|B|C`.
    /// Labels are kept as kind-name strings (not `NodeKind`) so eval is a plain
    /// per-row string compare. The parser resolves every label (category labels
    /// expand to member kinds) and refuses an unknown one, as the MATCH pattern does.
    HasLabel(String, Vec<String>),
    /// `EXISTS { pattern }` / `NOT EXISTS (pattern)` WHERE predicate.
    /// `negated` folds a leading NOT so the executor short-circuits on the
    /// first matching edge without an extra UnaryOp wrap. Outer-scope-bound
    /// vars in the pattern are fixed; unbound vars form the traversal frontier.
    ExistsPattern {
        pattern: Pattern,
        negated: bool,
    },
    /// `x IS [NOT] NULL`. Distinct from `BinOp(Eq, x, NULL)` because NULL
    /// comparison in Cypher is itself NULL (never true), so `= NULL` can't
    /// express the test the way `IS NULL` does.
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    FunCall {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
    },
    /// Postfix list subscript `base[idx]`. Negative `idx` counts from the end;
    /// out-of-range or non-list base evaluates to Null.
    Index(Box<Expr>, Box<Expr>),
    /// Postfix list slice `base[from..to]`; either bound may be omitted.
    Slice {
        base: Box<Expr>,
        from: Option<Box<Expr>>,
        to: Option<Box<Expr>>,
    },
}

#[derive(Debug, Clone)]
pub struct ReturnClause {
    pub distinct: bool,
    pub items: Vec<ReturnItem>,
}

#[derive(Debug, Clone)]
pub struct ReturnItem {
    pub expr: ReturnExpr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ReturnExpr {
    Star,
    Var(String),
    Prop(String, String),
    FunCall {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
    },
    /// Postfix-subscripted projection (`labels(n)[0]`, `n.list[1..]`); always
    /// an `Expr::Index` / `Expr::Slice`, evaluated through `eval_expr`.
    Subscript(Expr),
}

#[derive(Debug, Clone)]
pub struct OrderItem {
    pub expr: ReturnExpr,
    pub desc: bool,
}

#[derive(Debug, Clone)]
pub struct WithClause {
    pub items: Vec<ReturnItem>,
    pub where_: Option<Expr>,
}
