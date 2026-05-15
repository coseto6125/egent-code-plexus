use crate::cypher::ast::*;
use crate::cypher::error::CypherError;
use crate::cypher::value::{QueryResult, Value};
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One row of intermediate bindings during pattern matching.
#[derive(Debug, Clone, Default)]
struct Binding {
    /// var_name -> node index into `graph.nodes`
    node_vars: HashMap<String, u32>,
    /// var_name -> edge index into `graph.edges`
    edge_vars: HashMap<String, u32>,
}

/// Reading file content for `.content` projection (used by C12+).
#[allow(dead_code)]
struct ContentCache {
    repo_root: PathBuf,
    files: HashMap<u32, Option<String>>,
}

impl ContentCache {
    fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            files: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    fn body_for_file(&mut self, graph: &ArchivedZeroCopyGraph, file_idx: u32) -> Option<&str> {
        if !self.files.contains_key(&file_idx) {
            let body = if (file_idx as usize) < graph.files.len() {
                let rel = graph.files[file_idx as usize]
                    .path
                    .resolve(&graph.string_pool);
                std::fs::read_to_string(self.repo_root.join(rel)).ok()
            } else {
                None
            };
            self.files.insert(file_idx, body);
        }
        self.files.get(&file_idx).and_then(|o| o.as_deref())
    }
}

pub fn execute(
    query: &Query,
    graph: &ArchivedZeroCopyGraph,
    repo_root: &Path,
) -> Result<QueryResult, CypherError> {
    let mut cache = ContentCache::new(repo_root.to_path_buf());
    execute_inner(query, graph, &mut cache)
}

fn execute_inner(
    query: &Query,
    graph: &ArchivedZeroCopyGraph,
    cache: &mut ContentCache,
) -> Result<QueryResult, CypherError> {
    // Phase 1: produce bindings from MATCH clauses.
    let mut bindings: Vec<Binding> = vec![Binding::default()];
    for mc in &query.matches {
        bindings = exec_match_clause(mc, &bindings, graph)?;
    }

    // Phase 2: apply WHERE.
    if let Some(w) = &query.where_ {
        bindings.retain(|b| eval_expr(w, b, graph).map(value_truthy).unwrap_or(false));
    }

    // Phase 3: RETURN projection.
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Value>> = Vec::new();

    for b in &bindings {
        let mut row = Vec::new();
        for item in &query.return_.items {
            let (col_name, v) = project_item(item, b, graph, cache)?;
            if rows.is_empty() {
                columns.push(col_name);
            }
            row.push(v);
        }
        rows.push(row);
    }

    // Emit columns even when result set is empty.
    if rows.is_empty() {
        for item in &query.return_.items {
            columns.push(
                item.alias
                    .clone()
                    .unwrap_or_else(|| return_item_default_col(item)),
            );
        }
    }

    Ok(QueryResult { columns, rows })
}

fn exec_match_clause(
    mc: &MatchClause,
    prior: &[Binding],
    graph: &ArchivedZeroCopyGraph,
) -> Result<Vec<Binding>, CypherError> {
    let mut out = Vec::new();
    for pat in &mc.patterns {
        for b in prior {
            let extended = exec_pattern(pat, b, graph)?;
            if mc.optional && extended.is_empty() {
                // Left-join: keep left binding, vars from this pattern stay unset.
                out.push(b.clone());
            } else {
                out.extend(extended);
            }
        }
    }
    Ok(out)
}

/// Walk a pattern from left to right, producing one `Binding` per full match.
///
/// We carry an explicit `last_node_idx` alongside each partial binding so that
/// anonymous nodes (no var) still allow subsequent hops to advance correctly.
/// If the first node pattern has a variable that is already bound in `base`,
/// we seed from that single node rather than scanning all nodes.
fn exec_pattern(
    pat: &Pattern,
    base: &Binding,
    graph: &ArchivedZeroCopyGraph,
) -> Result<Vec<Binding>, CypherError> {
    // Frontier: (binding, last_matched_node_idx)
    let mut frontier: Vec<(Binding, u32)> = Vec::new();
    let first_np = &pat.nodes[0];

    // If the first node var is already bound, pin to that node only.
    if let Some(var) = &first_np.var {
        if let Some(&already) = base.node_vars.get(var) {
            let node = &graph.nodes[already as usize];
            if node_matches(node, first_np, graph) {
                frontier.push((base.clone(), already));
            }
        } else {
            for (idx, node) in graph.nodes.iter().enumerate() {
                if !node_matches(node, first_np, graph) {
                    continue;
                }
                let mut b = base.clone();
                b.node_vars.insert(var.clone(), idx as u32);
                frontier.push((b, idx as u32));
            }
        }
    } else {
        // Anonymous first node: scan all nodes.
        for (idx, node) in graph.nodes.iter().enumerate() {
            if !node_matches(node, first_np, graph) {
                continue;
            }
            frontier.push((base.clone(), idx as u32));
        }
    }

    for (hop, rel) in pat.rels.iter().enumerate() {
        let next_np = &pat.nodes[hop + 1];
        let mut next_frontier: Vec<(Binding, u32)> = Vec::new();

        for (b, cur_idx) in &frontier {
            for (tgt_idx, edge_idx) in walk_rel(*cur_idx, rel, graph) {
                let tgt_node = &graph.nodes[tgt_idx as usize];
                if !node_matches(tgt_node, next_np, graph) {
                    continue;
                }
                let mut nb = b.clone();
                if let Some(var) = &next_np.var {
                    nb.node_vars.insert(var.clone(), tgt_idx);
                }
                if let Some(var) = &rel.var {
                    nb.edge_vars.insert(var.clone(), edge_idx);
                }
                next_frontier.push((nb, tgt_idx));
            }
        }
        frontier = next_frontier;
    }

    Ok(frontier.into_iter().map(|(b, _)| b).collect())
}

fn node_matches(
    node: &crate::graph::ArchivedNode,
    np: &NodePat,
    graph: &ArchivedZeroCopyGraph,
) -> bool {
    if !np.kinds.is_empty() {
        let kind: NodeKind =
            rkyv::deserialize::<NodeKind, rkyv::rancor::Error>(&node.kind).unwrap();
        if !np.kinds.contains(&kind) {
            return false;
        }
    }
    for (key, lit) in &np.props {
        match key.as_str() {
            "name" => {
                let n = node.name.resolve(&graph.string_pool);
                if let Literal::Str(s) = lit {
                    if n != s.as_str() {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            "kind" => {
                let kind: NodeKind =
                    rkyv::deserialize::<NodeKind, rkyv::rancor::Error>(&node.kind).unwrap();
                if let Literal::Str(s) = lit {
                    if format!("{kind:?}") != *s {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Walk one hop from `from` in the given direction, returning `(target_node_idx, edge_idx)`.
fn walk_rel(from: u32, rel: &RelPat, graph: &ArchivedZeroCopyGraph) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let dir = rel.dir;

    let check_type = |edge: &crate::graph::ArchivedEdge| -> bool {
        if rel.types.is_empty() {
            return true;
        }
        let rt: RelType =
            rkyv::deserialize::<RelType, rkyv::rancor::Error>(&edge.rel_type).unwrap();
        rel.types.contains(&rt)
    };

    if matches!(dir, Direction::Out | Direction::Both) {
        let s = graph.out_offsets[from as usize].to_native() as usize;
        let e = graph.out_offsets[from as usize + 1].to_native() as usize;
        for (i, edge) in graph.edges[s..e].iter().enumerate() {
            if check_type(edge) {
                out.push((edge.target.to_native(), (s + i) as u32));
            }
        }
    }
    if matches!(dir, Direction::In | Direction::Both) {
        let s = graph.in_offsets[from as usize].to_native() as usize;
        let e = graph.in_offsets[from as usize + 1].to_native() as usize;
        for i in s..e {
            let edge_idx = graph.in_edge_idx[i].to_native();
            let edge = &graph.edges[edge_idx as usize];
            if check_type(edge) {
                out.push((edge.source.to_native(), edge_idx));
            }
        }
    }
    out
}

fn eval_expr(
    e: &Expr,
    b: &Binding,
    graph: &ArchivedZeroCopyGraph,
) -> Result<Value, CypherError> {
    use Expr::*;
    match e {
        Lit(l) => Ok(lit_to_value(l)),
        Var(var) => {
            if let Some(&idx) = b.node_vars.get(var) {
                Ok(Value::Str(
                    graph.nodes[idx as usize]
                        .name
                        .resolve(&graph.string_pool)
                        .to_string(),
                ))
            } else {
                Ok(Value::Null)
            }
        }
        Prop(var, prop) => Ok(prop_value(var, prop, b, graph)),
        BinOp(op, lhs, rhs) => {
            let lv = eval_expr(lhs, b, graph)?;
            let rv = eval_expr(rhs, b, graph)?;
            Ok(Value::Bool(eval_binop(*op, &lv, &rv)))
        }
        UnaryOp(_op, inner) => {
            let v = eval_expr(inner, b, graph)?;
            Ok(Value::Bool(!value_truthy(v)))
        }
        In(lhs, lits) => {
            let v = eval_expr(lhs, b, graph)?;
            Ok(Value::Bool(lits.iter().any(|l| values_eq(&v, &lit_to_value(l)))))
        }
        Regex(lhs, pat) => {
            let v = eval_expr(lhs, b, graph)?;
            let re = regex::Regex::new(pat)
                .map_err(|err| CypherError::Exec { msg: format!("bad regex: {err}") })?;
            Ok(Value::Bool(matches!(v, Value::Str(ref s) if re.is_match(s))))
        }
        StartsWith(lhs, p) => {
            let v = eval_expr(lhs, b, graph)?;
            Ok(Value::Bool(matches!(v, Value::Str(ref s) if s.starts_with(p.as_str()))))
        }
        EndsWith(lhs, p) => {
            let v = eval_expr(lhs, b, graph)?;
            Ok(Value::Bool(matches!(v, Value::Str(ref s) if s.ends_with(p.as_str()))))
        }
        Contains(lhs, p) => {
            let v = eval_expr(lhs, b, graph)?;
            Ok(Value::Bool(matches!(v, Value::Str(ref s) if s.contains(p.as_str()))))
        }
        FunCall { .. } => Err(CypherError::Exec {
            msg: "function calls in WHERE not yet supported".into(),
        }),
    }
}

fn lit_to_value(l: &Literal) -> Value {
    match l {
        Literal::Null => Value::Null,
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Int(i) => Value::Int(*i),
        Literal::Float(f) => Value::Float(*f),
        Literal::Str(s) => Value::Str(s.clone()),
        Literal::List(xs) => Value::List(xs.iter().map(lit_to_value).collect()),
    }
}

fn prop_value(var: &str, prop: &str, b: &Binding, graph: &ArchivedZeroCopyGraph) -> Value {
    if let Some(&idx) = b.node_vars.get(var) {
        let n = &graph.nodes[idx as usize];
        return match prop {
            "name" => Value::Str(n.name.resolve(&graph.string_pool).to_string()),
            "uid" => Value::Str(n.uid.resolve(&graph.string_pool).to_string()),
            "kind" => {
                let kind: NodeKind =
                    rkyv::deserialize::<NodeKind, rkyv::rancor::Error>(&n.kind).unwrap();
                Value::Str(format!("{kind:?}"))
            }
            "filePath" => {
                let fi = n.file_idx.to_native() as usize;
                Value::Str(if fi < graph.files.len() {
                    graph.files[fi].path.resolve(&graph.string_pool).to_string()
                } else {
                    String::new()
                })
            }
            _ => Value::Null,
        };
    }
    if let Some(&edge_idx) = b.edge_vars.get(var) {
        let e = &graph.edges[edge_idx as usize];
        return match prop {
            "confidence" => Value::Float(e.confidence.to_native() as f64),
            "reason" => Value::Str(e.reason.resolve(&graph.string_pool).to_string()),
            "rel_type" => {
                let rt: RelType =
                    rkyv::deserialize::<RelType, rkyv::rancor::Error>(&e.rel_type).unwrap();
                Value::Str(format!("{rt:?}"))
            }
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
        And => value_truthy(l.clone()) && value_truthy(r.clone()),
        Or => value_truthy(l.clone()) || value_truthy(r.clone()),
        Lt | Le | Gt | Ge => match (l, r) {
            (Value::Int(a), Value::Int(b)) => match op {
                Lt => a < b,
                Le => a <= b,
                Gt => a > b,
                Ge => a >= b,
                _ => false,
            },
            (Value::Float(a), Value::Float(b)) => match op {
                Lt => a < b,
                Le => a <= b,
                Gt => a > b,
                Ge => a >= b,
                _ => false,
            },
            (Value::Int(a), Value::Float(b)) => {
                let a = *a as f64;
                match op {
                    Lt => a < *b,
                    Le => a <= *b,
                    Gt => a > *b,
                    Ge => a >= *b,
                    _ => false,
                }
            }
            (Value::Float(a), Value::Int(b)) => {
                let b = *b as f64;
                match op {
                    Lt => *a < b,
                    Le => *a <= b,
                    Gt => *a > b,
                    Ge => *a >= b,
                    _ => false,
                }
            }
            (Value::Str(a), Value::Str(b)) => match op {
                Lt => a < b,
                Le => a <= b,
                Gt => a > b,
                Ge => a >= b,
                _ => false,
            },
            _ => false,
        },
    }
}

fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Int(i), Value::Float(f)) | (Value::Float(f), Value::Int(i)) => {
            *i as f64 == *f
        }
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

fn project_item(
    item: &ReturnItem,
    b: &Binding,
    graph: &ArchivedZeroCopyGraph,
    _cache: &mut ContentCache,
) -> Result<(String, Value), CypherError> {
    let col_name = item.alias.clone().unwrap_or_else(|| return_item_default_col(item));
    let v = match &item.expr {
        ReturnExpr::Prop(var, prop) => prop_value(var, prop, b, graph),
        ReturnExpr::Var(var) => {
            if let Some(&idx) = b.node_vars.get(var) {
                Value::Str(
                    graph.nodes[idx as usize]
                        .name
                        .resolve(&graph.string_pool)
                        .to_string(),
                )
            } else {
                Value::Null
            }
        }
        ReturnExpr::Star => Value::Null,
        ReturnExpr::FunCall { .. } => Value::Null,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cypher::parse;
    use crate::graph::{
        Edge, File, FileCategory, Node, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
    };
    use crate::pool::StringPool;

    // -----------------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------------

    /// Two-node fixture:
    ///   caller(0) -[:Calls]-> callee(1)
    fn build_two_node() -> Vec<u8> {
        let mut pool = StringPool::new();
        let caller_name = pool.add("caller");
        let callee_name = pool.add("callee");
        let file_path = pool.add("src/x.ts");
        let reason = pool.add("ast-call");
        let uid_a = pool.add("0:caller");
        let uid_b = pool.add("0:callee");

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: file_path,
                mtime: 0,
                content_hash: [0u8; 32],
                category: FileCategory::Source,
            }],
            nodes: vec![
                Node {
                    uid: uid_a,
                    name: caller_name,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (0, 0, 5, 1),
                    community_id: 0,
                },
                Node {
                    uid: uid_b,
                    name: callee_name,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (6, 0, 8, 1),
                    community_id: 0,
                },
            ],
            edges: vec![Edge {
                source: 0,
                target: 1,
                rel_type: RelType::Calls,
                confidence: 1.0,
                reason,
            }],
            out_offsets: vec![0, 1, 1],
            in_offsets: vec![0, 0, 1],
            in_edge_idx: vec![0],
            name_index: vec![],
            embeddings: None,
            process_start: 2,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec()
    }

    fn with_two<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_two_node();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    // -----------------------------------------------------------------------
    // C1 – scaffolding compile check
    // -----------------------------------------------------------------------

    #[test]
    fn scaffolding_compiles() {
        let _c = ContentCache::new(PathBuf::from("."));
        let _b = Binding::default();
    }

    // -----------------------------------------------------------------------
    // C2 – single-hop MATCH
    // -----------------------------------------------------------------------

    #[test]
    fn exec_single_hop_returns_one_row() {
        with_two(|g| {
            let q =
                parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, b.name").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.columns, vec!["a.name", "b.name"]);
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("caller".into()));
            assert_eq!(r.rows[0][1], Value::Str("callee".into()));
        });
    }

    #[test]
    fn exec_single_hop_with_where_name() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls]->(b:Function) WHERE a.name = 'caller' RETURN b.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("callee".into()));
        });
    }

    #[test]
    fn exec_single_hop_empty_result_emits_columns() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls]->(b:Function) WHERE a.name = 'nobody' RETURN a.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.columns, vec!["a.name"]);
            assert!(r.rows.is_empty());
        });
    }
}
