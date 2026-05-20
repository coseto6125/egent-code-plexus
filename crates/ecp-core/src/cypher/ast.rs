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
    Regex(Box<Expr>, String),
    StartsWith(Box<Expr>, String),
    EndsWith(Box<Expr>, String),
    Contains(Box<Expr>, String),
    FunCall {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
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
