use crate::cypher::ast::*;
use crate::cypher::error::CypherError;
use crate::cypher::value::{QueryResult, Value};
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One row of intermediate bindings during pattern matching.
#[derive(Debug, Clone, Default)]
struct Binding {
    /// var_name -> node index into `graph.nodes`
    node_vars: HashMap<String, u32>,
    /// var_name -> edge index into `graph.edges`
    edge_vars: HashMap<String, u32>,
    /// Values computed by a prior WITH clause. Checked before node_vars/edge_vars
    /// in prop_value and project_item.
    computed: HashMap<String, Value>,
}

/// Reading file content for `.content` projection, plus hot-path caches.
struct ContentCache {
    repo_root: PathBuf,
    files: HashMap<u32, Option<String>>,
    regex_cache: HashMap<String, regex::Regex>,
}

impl ContentCache {
    fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            files: HashMap::new(),
            regex_cache: HashMap::new(),
        }
    }

    fn body_for_file(&mut self, graph: &ArchivedZeroCopyGraph, file_idx: u32) -> Option<&str> {
        self.files
            .entry(file_idx)
            .or_insert_with(|| {
                if (file_idx as usize) < graph.files.len() {
                    let rel = graph.files[file_idx as usize]
                        .path
                        .resolve(&graph.string_pool);
                    std::fs::read_to_string(self.repo_root.join(rel)).ok()
                } else {
                    None
                }
            })
            .as_deref()
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
    // Produce bindings from MATCH clauses.
    let mut bindings: Vec<Binding> = vec![Binding::default()];
    for mc in &query.matches {
        bindings = exec_match_clause(mc, &bindings, graph)?;
    }

    // Apply WHERE filter.
    if let Some(w) = &query.where_ {
        // Collect retain mask separately to avoid simultaneous &mut borrows.
        // Propagate eval errors (e.g. uid string-literal misuse) to the caller.
        let mask: Vec<bool> = bindings
            .iter()
            .map(|b| eval_expr(w, b, graph, cache).map(|v| value_truthy(&v)))
            .collect::<Result<_, _>>()?;
        let mut mask_iter = mask.into_iter();
        bindings.retain(|_| mask_iter.next().unwrap_or(false));
    }

    // WITH clause rebinds / aggregates into a new binding set.
    if let Some(wc) = &query.with {
        bindings = exec_with(wc, bindings, graph, cache)?;
    }

    // Pre-expand bare Var RETURN items into concrete prop columns.
    // We use the first binding to infer whether each var is node/edge/computed-bound.
    let expanded_items: Vec<(String, ReturnExpr)> =
        expand_return_items(&query.return_.items, bindings.first())?;

    // RETURN projection — detect aggregation in expanded items. Scalar
    // function calls (`type(r)`, `id(n)`, `labels(n)`) are NOT aggregates and
    // must not trigger the group-by path; they project per-row in the else
    // branch via `eval_return_expr`.
    let has_agg = expanded_items
        .iter()
        .any(|(_, e)| matches!(e, ReturnExpr::FunCall { name, .. } if is_aggregate_fn(name)));

    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Value>> = Vec::new();

    if has_agg {
        // Partition expanded items into group-key items and aggregate items.
        let group_items: Vec<&(String, ReturnExpr)> = expanded_items
            .iter()
            .filter(
                |(_, e)| !matches!(e, ReturnExpr::FunCall { name, .. } if is_aggregate_fn(name)),
            )
            .collect();
        // Build column names.
        for (col, _) in &expanded_items {
            columns.push(col.clone());
        }

        // Group bindings by key values.
        let mut groups: Vec<(Vec<Value>, Vec<Binding>)> = Vec::new();
        let mut key_index: HashMap<String, usize> = HashMap::new();

        for b in &bindings {
            let key_vals: Result<Vec<Value>, CypherError> = group_items
                .iter()
                .map(|(_, e)| eval_return_expr(e, b, graph, cache))
                .collect();
            let key_vals = key_vals?;
            let key_str: String = key_vals
                .iter()
                .map(value_key)
                .collect::<Vec<_>>()
                .join("\x00");
            let entry = key_index.entry(key_str.clone()).or_insert_with(|| {
                groups.push((key_vals.clone(), Vec::new()));
                groups.len() - 1
            });
            groups[*entry].1.push(b.clone());
        }

        // If no bindings at all but RETURN has only aggregates (no group keys),
        // emit one row with COUNT=0 etc.
        if groups.is_empty() && group_items.is_empty() {
            groups.push((vec![], vec![]));
        }

        for (key_vals, group) in &groups {
            let mut row = Vec::new();
            let mut key_iter = key_vals.iter();
            for (_, expr) in &expanded_items {
                if let ReturnExpr::FunCall {
                    name,
                    distinct,
                    args,
                } = expr
                {
                    if is_aggregate_fn(name) {
                        let v = apply_aggregate(name, *distinct, args, group, graph, cache)?;
                        row.push(v);
                    } else {
                        // Scalar FunCall lives in group_items; its value is in key_vals.
                        row.push(key_iter.next().cloned().unwrap_or(Value::Null));
                    }
                } else {
                    row.push(key_iter.next().cloned().unwrap_or(Value::Null));
                }
            }
            rows.push(row);
        }
    } else {
        // No aggregation: simple row-by-row projection.
        columns = expanded_items.iter().map(|(col, _)| col.clone()).collect();
        for b in &bindings {
            let mut row = Vec::new();
            for (_, expr) in &expanded_items {
                row.push(eval_return_expr(expr, b, graph, cache)?);
            }
            rows.push(row);
        }
    }

    // ORDER BY.
    if !query.order_by.is_empty() {
        // Pre-build column index once rather than scanning per comparison.
        let col_index: HashMap<String, usize> = columns
            .iter()
            .enumerate()
            .map(|(i, c)| (c.clone(), i))
            .collect();
        rows.sort_by(|a, b| {
            for oi in &query.order_by {
                let col_name = match &oi.expr {
                    ReturnExpr::Prop(var, prop) => format!("{var}.{prop}"),
                    ReturnExpr::Var(v) => v.clone(),
                    ReturnExpr::Star => "*".into(),
                    ReturnExpr::FunCall { name, .. } => format!("{name}(*)"),
                };
                let col_idx = col_index.get(&col_name).copied();
                let av = col_idx.and_then(|i| a.get(i));
                let bv = col_idx.and_then(|i| b.get(i));
                let ord = cmp_values(av, bv);
                let ord = if oi.desc { ord.reverse() } else { ord };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
    }

    // DISTINCT dedup.
    if query.return_.distinct {
        dedup_rows(&mut rows);
    }

    // SKIP + LIMIT.
    let skip = query.skip.unwrap_or(0) as usize;
    if skip > 0 {
        rows = rows.into_iter().skip(skip).collect();
    }
    if let Some(lim) = query.limit {
        rows.truncate(lim as usize);
    }

    // UNION / UNION ALL.
    if let Some(union_query) = &query.union {
        let right = execute_inner(union_query, graph, cache)?;
        if right.columns.len() != columns.len() {
            return Err(CypherError::Semantic {
                msg: "UNION column count mismatch".into(),
            });
        }
        rows.extend(right.rows);
        if !query.union_all {
            dedup_rows(&mut rows);
        }
    }

    Ok(QueryResult { columns, rows })
}

fn dedup_rows(rows: &mut Vec<Vec<Value>>) {
    let mut seen = HashSet::new();
    rows.retain(|row| seen.insert(format!("{row:?}")));
}

/// Compare two optional row cell values for ORDER BY sorting.
fn cmp_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(av), Some(bv)) => cmp_value_pair(av, bv),
    }
}

fn cmp_value_pair(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        // Null sorts before everything.
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Less,
        (_, Value::Null) => Greater,
        // Bool: false < true.
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        // Int-Int.
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        // Float-Float.
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Equal),
        // Int-Float promotion.
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
        // Str lexicographic.
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        // Fallback: debug repr comparison.
        _ => format!("{a:?}").cmp(&format!("{b:?}")),
    }
}

/// Expand RETURN items: replace bare `Var(name)` with 3 concrete prop columns
/// when `name` is node-bound (name/kind/filePath) or edge-bound (rel_type/confidence/reason).
/// Computed and unbound vars are kept as a single column.
fn expand_return_items(
    items: &[ReturnItem],
    first_binding: Option<&Binding>,
) -> Result<Vec<(String, ReturnExpr)>, CypherError> {
    let mut out: Vec<(String, ReturnExpr)> = Vec::new();
    for item in items {
        match &item.expr {
            ReturnExpr::Var(name) => {
                // Check if it's a computed binding first.
                let is_computed = first_binding
                    .map(|b| b.computed.contains_key(name))
                    .unwrap_or(false);
                let is_node = first_binding
                    .map(|b| b.node_vars.contains_key(name))
                    .unwrap_or(false);
                let is_edge = first_binding
                    .map(|b| b.edge_vars.contains_key(name))
                    .unwrap_or(false);

                // If aliased with AS, treat as single column regardless of binding type.
                if let Some(col) = &item.alias {
                    out.push((col.clone(), item.expr.clone()));
                } else if is_computed {
                    // Single computed column.
                    out.push((name.clone(), item.expr.clone()));
                } else if is_node {
                    // Expand node var into 3 columns: .name, .kind, .filePath
                    out.push((
                        format!("{name}.name"),
                        ReturnExpr::Prop(name.clone(), "name".into()),
                    ));
                    out.push((
                        format!("{name}.kind"),
                        ReturnExpr::Prop(name.clone(), "kind".into()),
                    ));
                    out.push((
                        format!("{name}.filePath"),
                        ReturnExpr::Prop(name.clone(), "filePath".into()),
                    ));
                } else if is_edge {
                    // Expand edge var into 3 columns: .rel_type, .confidence, .reason
                    out.push((
                        format!("{name}.rel_type"),
                        ReturnExpr::Prop(name.clone(), "rel_type".into()),
                    ));
                    out.push((
                        format!("{name}.confidence"),
                        ReturnExpr::Prop(name.clone(), "confidence".into()),
                    ));
                    out.push((
                        format!("{name}.reason"),
                        ReturnExpr::Prop(name.clone(), "reason".into()),
                    ));
                } else if first_binding.is_some() {
                    // Bound binding exists but var is not in it — semantic error.
                    return Err(CypherError::Semantic {
                        msg: format!("unbound variable '{name}'"),
                    });
                } else {
                    // No bindings at all (empty result set) — emit as-is.
                    out.push((name.clone(), item.expr.clone()));
                }
            }
            _ => {
                let col = item
                    .alias
                    .clone()
                    .unwrap_or_else(|| return_item_default_col(item));
                out.push((col, item.expr.clone()));
            }
        }
    }
    Ok(out)
}

/// Evaluate a ReturnExpr directly against a binding (used in the non-agg projection path).
fn eval_return_expr(
    expr: &ReturnExpr,
    b: &Binding,
    graph: &ArchivedZeroCopyGraph,
    cache: &mut ContentCache,
) -> Result<Value, CypherError> {
    match expr {
        ReturnExpr::Prop(var, prop) => Ok(prop_value(var, prop, b, graph, cache)),
        ReturnExpr::Var(var) => {
            if let Some(v) = b.computed.get(var) {
                Ok(v.clone())
            } else if let Some(&idx) = b.node_vars.get(var) {
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
        ReturnExpr::Star => Ok(Value::Null),
        ReturnExpr::FunCall { name, args, .. } => {
            // Aggregate FunCalls reach this path only when the caller already
            // verified there is no aggregate in the projection — so treating
            // them as scalar is a safe no-op (returns Null).
            Ok(eval_scalar_funcall(name, args, b, graph))
        }
    }
}

/// Stable string key for a Value (used as group-by key; avoids Hash on Value).
fn value_key(v: &Value) -> String {
    format!("{v:?}")
}

/// Evaluate a ReturnItem's expression into a Value, preserving NodeRef/EdgeRef
/// for variables bound to graph nodes/edges. Used by WITH group-key computation
/// so that `a.name` still resolves after aggregation clears node_vars.
fn eval_return_item_rich(
    item: &ReturnItem,
    b: &Binding,
    graph: &ArchivedZeroCopyGraph,
    cache: &mut ContentCache,
) -> Value {
    match &item.expr {
        ReturnExpr::Var(var) => {
            // Check computed first.
            if let Some(v) = b.computed.get(var) {
                return v.clone();
            }
            if let Some(&idx) = b.node_vars.get(var) {
                let n = &graph.nodes[idx as usize];
                let fi = n.file_idx.to_native() as usize;
                let file_path = if fi < graph.files.len() {
                    graph.files[fi].path.resolve(&graph.string_pool).to_string()
                } else {
                    String::new()
                };
                return Value::NodeRef {
                    idx,
                    name: n.name.resolve(&graph.string_pool).to_string(),
                    kind: archived_kind_str(n).to_string(),
                    file_path,
                };
            }
            if let Some(&eidx) = b.edge_vars.get(var) {
                let e = &graph.edges[eidx as usize];
                let rt: crate::graph::RelType =
                    rkyv::deserialize::<crate::graph::RelType, rkyv::rancor::Error>(&e.rel_type)
                        .unwrap();
                return Value::EdgeRef {
                    src: e.source.to_native(),
                    tgt: e.target.to_native(),
                    rel_type: rt,
                    confidence: e.confidence.to_native(),
                    reason: e.reason.resolve(&graph.string_pool).to_string(),
                };
            }
            Value::Null
        }
        ReturnExpr::Prop(var, prop) => prop_value(var, prop, b, graph, cache),
        ReturnExpr::Star => Value::Null,
        ReturnExpr::FunCall { name, args, .. } => eval_scalar_funcall(name, args, b, graph),
    }
}

/// Execute a WITH clause: rebind plain items into `computed`, or group+aggregate.
fn exec_with(
    wc: &WithClause,
    bindings: Vec<Binding>,
    graph: &ArchivedZeroCopyGraph,
    cache: &mut ContentCache,
) -> Result<Vec<Binding>, CypherError> {
    let has_agg = wc
        .items
        .iter()
        .any(|i| matches!(&i.expr, ReturnExpr::FunCall { name, .. } if is_aggregate_fn(name)));

    let mut out: Vec<Binding> = if has_agg {
        // Partition WITH items into group-key items and aggregate items.
        // Scalar FunCalls (`type(r)` etc.) stay in `group_items` so they
        // contribute to the grouping key and emit per-row in the result.
        let group_items: Vec<&ReturnItem> = wc
            .items
            .iter()
            .filter(
                |i| !matches!(&i.expr, ReturnExpr::FunCall { name, .. } if is_aggregate_fn(name)),
            )
            .collect();
        let agg_items: Vec<&ReturnItem> = wc
            .items
            .iter()
            .filter(
                |i| matches!(&i.expr, ReturnExpr::FunCall { name, .. } if is_aggregate_fn(name)),
            )
            .collect();

        type GroupEntry = (Vec<(String, Value)>, Vec<Binding>);
        // Group bindings by key values.
        let mut groups: Vec<GroupEntry> = Vec::new();
        let mut key_index: HashMap<String, usize> = HashMap::new();

        for b in &bindings {
            let key_pairs: Vec<(String, Value)> = group_items
                .iter()
                .map(|gi| {
                    let col = gi
                        .alias
                        .clone()
                        .unwrap_or_else(|| return_item_default_col(gi));
                    let v = eval_return_item_rich(gi, b, graph, cache);
                    (col, v)
                })
                .collect();
            let key_str: String = key_pairs
                .iter()
                .map(|(_, v)| value_key(v))
                .collect::<Vec<_>>()
                .join("\x00");
            let entry = key_index.entry(key_str).or_insert_with(|| {
                groups.push((key_pairs.clone(), Vec::new()));
                groups.len() - 1
            });
            groups[*entry].1.push(b.clone());
        }

        // Produce one output Binding per group.
        let mut result = Vec::with_capacity(groups.len());
        for (key_pairs, group) in &groups {
            let mut computed: HashMap<String, Value> = HashMap::new();
            for (col, val) in key_pairs {
                computed.insert(col.clone(), val.clone());
            }
            for ai in &agg_items {
                let col = ai
                    .alias
                    .clone()
                    .unwrap_or_else(|| return_item_default_col(ai));
                if let ReturnExpr::FunCall {
                    name,
                    distinct,
                    args,
                } = &ai.expr
                {
                    let v = apply_aggregate(name, *distinct, args, group, graph, cache)?;
                    computed.insert(col, v);
                }
            }
            result.push(Binding {
                node_vars: HashMap::new(),
                edge_vars: HashMap::new(),
                computed,
            });
        }
        result
    } else {
        // Plain rebinding: no aggregation. Preserve node_vars/edge_vars so that
        // subsequent MATCH clauses can still traverse them.
        let mut result = Vec::with_capacity(bindings.len());
        for b in &bindings {
            let mut computed: HashMap<String, Value> = HashMap::new();
            for item in &wc.items {
                let col = item
                    .alias
                    .clone()
                    .unwrap_or_else(|| return_item_default_col(item));
                // Use rich projection to preserve NodeRef/EdgeRef identity.
                let v = eval_return_item_rich(item, b, graph, cache);
                computed.insert(col, v);
            }
            result.push(Binding {
                node_vars: b.node_vars.clone(),
                edge_vars: b.edge_vars.clone(),
                computed,
            });
        }
        result
    };

    // Apply inner WHERE of WITH clause (filters post-aggregation output).
    if let Some(w) = &wc.where_ {
        let mask: Vec<bool> = out
            .iter()
            .map(|b| eval_expr(w, b, graph, cache).map(|v| value_truthy(&v)))
            .collect::<Result<_, _>>()?;
        let mut mask_iter = mask.into_iter();
        out.retain(|_| mask_iter.next().unwrap_or(false));
    }

    Ok(out)
}

/// Names recognized as aggregate functions. Anything else parsed as a
/// FunCall is treated as a scalar function (`type(r)`, `id(n)`, `labels(n)`).
/// Pre-uppercased — the parser normalizes (`parser.rs:382/398/572/588`).
fn is_aggregate_fn(name: &str) -> bool {
    matches!(name, "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "COLLECT")
}

/// Evaluate a scalar (non-aggregate) function call. Returns `Value::Null` for
/// unknown functions rather than erroring — matches the OpenCypher convention
/// that missing-data scalars degrade gracefully (see graph_query rel-type
/// `matches!` path). Supports the three functions LLM agents reach for most:
/// `type(r)` → edge rel-type as Str; `id(n)` → node index as Int;
/// `labels(n)` → single-element list of node-kind Str.
fn eval_scalar_funcall(
    name: &str,
    args: &[Expr],
    b: &Binding,
    graph: &ArchivedZeroCopyGraph,
) -> Value {
    match name {
        "TYPE" => {
            // type(r) — args[0] must be a Var bound to an edge.
            let Some(Expr::Var(var)) = args.first() else {
                return Value::Null;
            };
            let Some(&eidx) = b.edge_vars.get(var) else {
                return Value::Null;
            };
            let e = &graph.edges[eidx as usize];
            Value::Str(RelType::from(&e.rel_type).as_str().to_string())
        }
        "ID" => {
            // id(n) — args[0] must be a Var bound to a node.
            let Some(Expr::Var(var)) = args.first() else {
                return Value::Null;
            };
            let Some(&idx) = b.node_vars.get(var) else {
                return Value::Null;
            };
            Value::Int(idx as i64)
        }
        "LABELS" => {
            // labels(n) — single-kind list per ecp's one-label-per-node model.
            let Some(Expr::Var(var)) = args.first() else {
                return Value::Null;
            };
            let Some(&idx) = b.node_vars.get(var) else {
                return Value::Null;
            };
            Value::List(vec![Value::Str(
                archived_kind_str(&graph.nodes[idx as usize]).to_string(),
            )])
        }
        _ => Value::Null,
    }
}

/// Evaluate one aggregate function over a group of bindings.
fn apply_aggregate(
    name: &str,
    distinct: bool,
    args: &[Expr],
    group: &[Binding],
    graph: &ArchivedZeroCopyGraph,
    cache: &mut ContentCache,
) -> Result<Value, CypherError> {
    // COUNT(*) sentinel: args = [Lit(Null)]
    let is_count_star = matches!(args, [Expr::Lit(Literal::Null)]);

    match name {
        "COUNT" => {
            if is_count_star {
                return Ok(Value::Int(group.len() as i64));
            }
            let arg = &args[0];
            if distinct {
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                let mut cnt = 0i64;
                for b in group {
                    let v = eval_expr(arg, b, graph, cache)?;
                    if !matches!(v, Value::Null) {
                        let k = value_key(&v);
                        if seen.insert(k) {
                            cnt += 1;
                        }
                    }
                }
                Ok(Value::Int(cnt))
            } else {
                let mut cnt = 0i64;
                for b in group {
                    let v = eval_expr(arg, b, graph, cache)?;
                    if !matches!(v, Value::Null) {
                        cnt += 1;
                    }
                }
                Ok(Value::Int(cnt))
            }
        }
        "SUM" => {
            let arg = &args[0];
            let mut sum_i: i64 = 0;
            let mut sum_f: f64 = 0.0;
            let mut has_float = false;
            for b in group {
                match eval_expr(arg, b, graph, cache)? {
                    Value::Int(i) => sum_i += i,
                    Value::Float(f) => {
                        sum_f += f;
                        has_float = true;
                    }
                    _ => {}
                }
            }
            if has_float {
                Ok(Value::Float(sum_f + sum_i as f64))
            } else {
                Ok(Value::Int(sum_i))
            }
        }
        "MIN" => {
            let arg = &args[0];
            let mut min: Option<Value> = None;
            for b in group {
                let v = eval_expr(arg, b, graph, cache)?;
                if matches!(v, Value::Null) {
                    continue;
                }
                min = Some(match min {
                    None => v,
                    Some(cur) => {
                        if eval_binop(Op::Lt, &v, &cur) {
                            v
                        } else {
                            cur
                        }
                    }
                });
            }
            Ok(min.unwrap_or(Value::Null))
        }
        "MAX" => {
            let arg = &args[0];
            let mut max: Option<Value> = None;
            for b in group {
                let v = eval_expr(arg, b, graph, cache)?;
                if matches!(v, Value::Null) {
                    continue;
                }
                max = Some(match max {
                    None => v,
                    Some(cur) => {
                        if eval_binop(Op::Gt, &v, &cur) {
                            v
                        } else {
                            cur
                        }
                    }
                });
            }
            Ok(max.unwrap_or(Value::Null))
        }
        "AVG" => {
            let arg = &args[0];
            let mut sum: f64 = 0.0;
            let mut cnt: i64 = 0;
            for b in group {
                match eval_expr(arg, b, graph, cache)? {
                    Value::Int(i) => {
                        sum += i as f64;
                        cnt += 1;
                    }
                    Value::Float(f) => {
                        sum += f;
                        cnt += 1;
                    }
                    _ => {}
                }
            }
            if cnt == 0 {
                Ok(Value::Null)
            } else {
                Ok(Value::Float(sum / cnt as f64))
            }
        }
        "COLLECT" => {
            let arg = &args[0];
            let mut items: Vec<Value> = Vec::new();
            let mut seen: Option<std::collections::HashSet<String>> = if distinct {
                Some(std::collections::HashSet::new())
            } else {
                None
            };
            for b in group {
                let v = eval_expr(arg, b, graph, cache)?;
                if matches!(v, Value::Null) {
                    continue;
                }
                if let Some(ref mut s) = seen {
                    if !s.insert(value_key(&v)) {
                        continue;
                    }
                }
                items.push(v);
            }
            Ok(Value::List(items))
        }
        _ => Err(CypherError::Exec {
            msg: format!("unknown aggregate function '{name}'"),
        }),
    }
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
            match rel.range {
                // Variable-length BFS (*min..max)
                Some((min, max)) => {
                    let reached = bfs_var_len(*cur_idx, rel, graph, min, max);
                    for (tgt_idx, edge_idx_opt) in reached {
                        let tgt_node = &graph.nodes[tgt_idx as usize];
                        if !node_matches(tgt_node, next_np, graph) {
                            continue;
                        }
                        let mut nb = b.clone();
                        if let Some(var) = &next_np.var {
                            nb.node_vars.insert(var.clone(), tgt_idx);
                        }
                        if let Some(var) = &rel.var {
                            if let Some(ei) = edge_idx_opt {
                                nb.edge_vars.insert(var.clone(), ei);
                            }
                        }
                        next_frontier.push((nb, tgt_idx));
                    }
                }
                // Single-hop
                None => {
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
            }
        }
        frontier = next_frontier;
    }

    Ok(frontier.into_iter().map(|(b, _)| b).collect())
}

/// BFS for variable-length relationships `*min..max`.
/// Returns `(target_node_idx, last_edge_idx_option)` pairs reachable within depth range.
fn bfs_var_len(
    start: u32,
    rel: &RelPat,
    graph: &ArchivedZeroCopyGraph,
    min: u32,
    max: u32,
) -> Vec<(u32, Option<u32>)> {
    use std::collections::VecDeque;
    let mut visited = std::collections::HashSet::new();
    // queue: (node_idx, depth, last_edge_idx)
    let mut queue: VecDeque<(u32, u32, Option<u32>)> = VecDeque::new();
    queue.push_back((start, 0, None));
    visited.insert(start);

    let mut out = Vec::new();

    while let Some((idx, depth, last_edge)) = queue.pop_front() {
        if depth >= min {
            out.push((idx, last_edge));
        }
        if depth >= max {
            continue;
        }
        for (tgt, edge_idx) in walk_rel(idx, rel, graph) {
            if visited.insert(tgt) {
                queue.push_back((tgt, depth + 1, Some(edge_idx)));
            }
        }
    }
    out
}

fn node_matches(
    node: &crate::graph::ArchivedNode,
    np: &NodePat,
    graph: &ArchivedZeroCopyGraph,
) -> bool {
    // Zero-cost discriminant read; reused by both label filter and `kind` prop filter below.
    let kind: NodeKind = NodeKind::from(&node.kind);
    if !np.kinds.is_empty() && !np.kinds.contains(&kind) {
        return false;
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
                if let Literal::Str(s) = lit {
                    if kind.as_str() != s {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            "uid" => {
                if let Literal::Int(v) = lit {
                    if node.uid.to_native() as i64 != *v {
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
        rel.types.contains(&RelType::from(&edge.rel_type))
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
    cache: &mut ContentCache,
) -> Result<Value, CypherError> {
    use Expr::*;
    match e {
        Lit(l) => Ok(lit_to_value(l)),
        Var(var) => {
            // Check computed values from WITH clause first.
            if let Some(v) = b.computed.get(var) {
                return Ok(v.clone());
            }
            if let Some(&idx) = b.node_vars.get(var) {
                return Ok(Value::Str(
                    graph.nodes[idx as usize]
                        .name
                        .resolve(&graph.string_pool)
                        .to_string(),
                ));
            }
            // Edge variables must resolve to non-Null so aggregates like
            // `count(r)` and `count(DISTINCT r)` see a value per binding.
            // Returns EdgeRef (same shape as the rich projection path uses)
            // so `value_key` partitions on edge identity for DISTINCT.
            if let Some(&eidx) = b.edge_vars.get(var) {
                let e = &graph.edges[eidx as usize];
                return Ok(Value::EdgeRef {
                    src: e.source.to_native(),
                    tgt: e.target.to_native(),
                    rel_type: RelType::from(&e.rel_type),
                    confidence: e.confidence.to_native(),
                    reason: e.reason.resolve(&graph.string_pool).to_string(),
                });
            }
            Ok(Value::Null)
        }
        Prop(var, prop) => Ok(prop_value(var, prop, b, graph, cache)),
        BinOp(op, lhs, rhs) => {
            // Catch `n.uid = "string"` before evaluation to give a clear error.
            let uid_str_side = |e: &Expr| matches!(e, Prop(_, p) if p == "uid");
            let str_lit_side = |e: &Expr| matches!(e, Lit(Literal::Str(_)));
            if (uid_str_side(lhs) && str_lit_side(rhs)) || (str_lit_side(lhs) && uid_str_side(rhs))
            {
                return Err(CypherError::Exec {
                    msg: "n.uid is u64; pass a numeric literal, not a string".into(),
                });
            }
            let lv = eval_expr(lhs, b, graph, cache)?;
            let rv = eval_expr(rhs, b, graph, cache)?;
            Ok(Value::Bool(eval_binop(*op, &lv, &rv)))
        }
        UnaryOp(_op, inner) => {
            let v = eval_expr(inner, b, graph, cache)?;
            Ok(Value::Bool(!value_truthy(&v)))
        }
        In(lhs, lits) => {
            let v = eval_expr(lhs, b, graph, cache)?;
            Ok(Value::Bool(
                lits.iter().any(|l| values_eq(&v, &lit_to_value(l))),
            ))
        }
        InCollection(scalar, collection) => {
            let needle = eval_expr(scalar, b, graph, cache)?;
            let haystack = eval_expr(collection, b, graph, cache)?;
            Ok(Value::Bool(match &haystack {
                Value::List(items) => items.iter().any(|item| values_eq(&needle, item)),
                _ => false,
            }))
        }
        Regex(lhs, pat) => {
            let v = eval_expr(lhs, b, graph, cache)?;
            if !cache.regex_cache.contains_key(pat) {
                let r = regex::Regex::new(pat).map_err(|e| CypherError::Exec {
                    msg: format!("bad regex: {e}"),
                })?;
                cache.regex_cache.insert(pat.clone(), r);
            }
            let re = cache.regex_cache.get(pat).unwrap();
            Ok(Value::Bool(
                matches!(v, Value::Str(ref s) if re.is_match(s)),
            ))
        }
        StartsWith(lhs, p) => {
            let v = eval_expr(lhs, b, graph, cache)?;
            Ok(Value::Bool(
                matches!(v, Value::Str(ref s) if s.starts_with(p.as_str())),
            ))
        }
        EndsWith(lhs, p) => {
            let v = eval_expr(lhs, b, graph, cache)?;
            Ok(Value::Bool(
                matches!(v, Value::Str(ref s) if s.ends_with(p.as_str())),
            ))
        }
        Contains(lhs, p) => {
            let v = eval_expr(lhs, b, graph, cache)?;
            Ok(Value::Bool(
                matches!(v, Value::Str(ref s) if s.contains(p.as_str())),
            ))
        }
        HasLabel(var, labels) => {
            // Unbound (edge var / WITH scalar) returns Null, mirroring the
            // `Var` arm's unbound convention so WHERE serialization and
            // value_truthy semantics stay consistent.
            let Some(&idx) = b.node_vars.get(var) else {
                return Ok(Value::Null);
            };
            let kind_str = archived_kind_str(&graph.nodes[idx as usize]);
            Ok(Value::Bool(labels.iter().any(|l| l == kind_str)))
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

/// Slice `source` by a tree-sitter style `(start_row, start_col, end_row, end_col)` span.
/// Rows/cols are 0-indexed; columns count UTF-8 bytes. Returns empty string on out-of-range.
fn slice_by_span(source: &str, span: (u32, u32, u32, u32)) -> String {
    let (start_row, start_col, end_row, end_col) = (
        span.0 as usize,
        span.1 as usize,
        span.2 as usize,
        span.3 as usize,
    );
    let lines: Vec<&str> = source.split('\n').collect();
    if start_row >= lines.len() || end_row >= lines.len() || start_row > end_row {
        return String::new();
    }
    if start_row == end_row {
        let line = lines[start_row].as_bytes();
        if start_col > line.len() || end_col > line.len() || start_col > end_col {
            return String::new();
        }
        return String::from_utf8_lossy(&line[start_col..end_col]).into_owned();
    }
    let mut out = String::new();
    let first = lines[start_row].as_bytes();
    let sc = start_col.min(first.len());
    out.push_str(&String::from_utf8_lossy(&first[sc..]));
    out.push('\n');
    for line in &lines[start_row + 1..end_row] {
        out.push_str(line);
        out.push('\n');
    }
    let last = lines[end_row].as_bytes();
    let ec = end_col.min(last.len());
    out.push_str(&String::from_utf8_lossy(&last[..ec]));
    out
}

fn prop_value(
    var: &str,
    prop: &str,
    b: &Binding,
    graph: &ArchivedZeroCopyGraph,
    cache: &mut ContentCache,
) -> Value {
    // Check computed values first (set by WITH clause).
    if let Some(computed_val) = b.computed.get(var) {
        return match computed_val {
            // If the computed value is a NodeRef, resolve the property from the graph.
            Value::NodeRef { idx, .. } => {
                let n = &graph.nodes[*idx as usize];
                node_prop_value(n, *idx, prop, graph, cache)
            }
            // EdgeRef: resolve edge properties.
            Value::EdgeRef {
                src: _,
                tgt: _,
                rel_type,
                confidence,
                reason,
            } => match prop {
                "confidence" => Value::Float(*confidence as f64),
                "reason" => Value::Str(reason.clone()),
                "rel_type" => Value::Str(format!("{rel_type:?}")),
                _ => Value::Null,
            },
            // Scalar: only bare var reference makes sense; <var>.<prop> returns Null.
            _ => {
                if prop.is_empty() {
                    computed_val.clone()
                } else {
                    Value::Null
                }
            }
        };
    }
    if let Some(&idx) = b.node_vars.get(var) {
        let n = &graph.nodes[idx as usize];
        return node_prop_value(n, idx, prop, graph, cache);
    }
    if let Some(&edge_idx) = b.edge_vars.get(var) {
        let e = &graph.edges[edge_idx as usize];
        return match prop {
            "confidence" => Value::Float(e.confidence.to_native() as f64),
            "reason" => Value::Str(e.reason.resolve(&graph.string_pool).to_string()),
            "rel_type" => Value::Str(RelType::from(&e.rel_type).as_str().to_string()),
            _ => Value::Null,
        };
    }
    Value::Null
}

/// Zero-cost archived-kind → static variant name (`"Function"`, `"Class"`, …).
/// Shared by the NodeRef projection, the `n.kind` property, and the WHERE
/// label-test arm so a future Display tweak on `NodeKind` lands at one site
/// instead of three.
fn archived_kind_str(node: &crate::graph::ArchivedNode) -> &'static str {
    NodeKind::from(&node.kind).as_str()
}

/// Resolve a single property from an archived node.
/// `node_idx` is the position of `n` in `graph.nodes` — needed for the sparse
/// `function_metas` binary-search lookup.
/// `cache` is used for the `content` property (C12).
fn node_prop_value(
    n: &crate::graph::ArchivedNode,
    node_idx: u32,
    prop: &str,
    graph: &ArchivedZeroCopyGraph,
    cache: &mut ContentCache,
) -> Value {
    match prop {
        "name" => Value::Str(n.name.resolve(&graph.string_pool).to_string()),
        // u64 uid stored as i64 bits — no allocation per row.
        "uid" => Value::Int(n.uid.to_native() as i64),
        "ownerClass" => {
            let oc = n.owner_class.resolve(&graph.string_pool);
            if oc.is_empty() {
                Value::Null
            } else {
                Value::Str(oc.to_string())
            }
        }
        "kind" => Value::Str(archived_kind_str(n).to_string()),
        "filePath" => {
            let fi = n.file_idx.to_native() as usize;
            Value::Str(if fi < graph.files.len() {
                graph.files[fi].path.resolve(&graph.string_pool).to_string()
            } else {
                String::new()
            })
        }
        "content" => {
            // Lazy file read + span slice.
            let file_idx = n.file_idx.to_native();
            let span = (
                n.span.0.to_native(),
                n.span.1.to_native(),
                n.span.2.to_native(),
                n.span.3.to_native(),
            );
            let slice = cache
                .body_for_file(graph, file_idx)
                .map(|body| slice_by_span(body, span))
                .unwrap_or_default();
            Value::Str(slice)
        }
        // ── FunctionMeta flag properties ────────────────────────────────────
        // FunctionMeta is sparse (only Function/Method/Constructor nodes).
        // Nodes without a record return safe defaults (false / 0 / empty list)
        // so WHERE m.is_async = true works without needing a Null check.
        "is_test" | "isTest" => Value::Bool(archived_fm_flag(
            graph,
            node_idx,
            crate::graph::FunctionMeta::FLAG_TEST,
        )),
        "is_async" | "isAsync" => Value::Bool(archived_fm_flag(
            graph,
            node_idx,
            crate::graph::FunctionMeta::FLAG_ASYNC,
        )),
        "is_static" | "isStatic" => Value::Bool(archived_fm_flag(
            graph,
            node_idx,
            crate::graph::FunctionMeta::FLAG_STATIC,
        )),
        "is_abstract" | "isAbstract" => Value::Bool(archived_fm_flag(
            graph,
            node_idx,
            crate::graph::FunctionMeta::FLAG_ABSTRACT,
        )),
        "is_generator" | "isGenerator" => Value::Bool(archived_fm_flag(
            graph,
            node_idx,
            crate::graph::FunctionMeta::FLAG_GENERATOR,
        )),
        "is_extern" | "isExtern" => Value::Bool(archived_fm_flag(
            graph,
            node_idx,
            crate::graph::FunctionMeta::FLAG_EXTERN,
        )),
        "visibility" => Value::Int(archived_fm_visibility(graph, node_idx) as i64),
        "decorators" => archived_fm_decorators(graph, node_idx),
        _ => Value::Null,
    }
}

/// Return true when the node's FunctionMeta has the given flag set.
/// Nodes with no FunctionMeta record return false (sparse-record default).
fn archived_fm_flag(graph: &ArchivedZeroCopyGraph, node_idx: u32, flag: u16) -> bool {
    match graph
        .function_metas
        .binary_search_by_key(&node_idx, |m| m.node_idx.to_native())
    {
        Ok(i) => graph.function_metas[i].flags.to_native() & flag != 0,
        Err(_) => false,
    }
}

/// Return the 3-bit visibility code for the node's FunctionMeta.
/// Nodes with no FunctionMeta record return 0 (public default).
fn archived_fm_visibility(graph: &ArchivedZeroCopyGraph, node_idx: u32) -> u8 {
    match graph
        .function_metas
        .binary_search_by_key(&node_idx, |m| m.node_idx.to_native())
    {
        Ok(i) => ((graph.function_metas[i].flags.to_native() >> 6) & 0b111) as u8,
        Err(_) => 0,
    }
}

/// Return the decorators list for the node's FunctionMeta.
/// Decorator names are normalized: leading `@` stripped so Python `app.get`
/// and Java `@Override` are both queryable as `Override` / `app.get`.
/// Nodes with no FunctionMeta record return an empty list.
/// TODO: the per-row Vec allocation here is unavoidable with the current
/// Value::List representation; profile if decorators filtering becomes a hotspot.
fn archived_fm_decorators(graph: &ArchivedZeroCopyGraph, node_idx: u32) -> Value {
    let items = match graph
        .function_metas
        .binary_search_by_key(&node_idx, |m| m.node_idx.to_native())
    {
        Ok(i) => graph.function_metas[i]
            .decorators
            .iter()
            .map(|d| {
                let s = d.resolve(&graph.string_pool);
                let normalized = s.strip_prefix('@').unwrap_or(s);
                Value::Str(normalized.to_string())
            })
            .collect(),
        Err(_) => vec![],
    };
    Value::List(items)
}

fn eval_binop(op: Op, l: &Value, r: &Value) -> bool {
    use Op::*;
    match op {
        Eq => values_eq(l, r),
        Ne => !values_eq(l, r),
        And => value_truthy(l) && value_truthy(r),
        Or => value_truthy(l) || value_truthy(r),
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
        (Value::Int(i), Value::Float(f)) | (Value::Float(f), Value::Int(i)) => *i as f64 == *f,
        _ => false,
    }
}

fn value_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        _ => true,
    }
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
    use crate::pool::{StrRef, StringPool};

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

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: file_path,
                mtime: 0,
                content_hash: [0u8; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "caller"),
                    name: caller_name,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (0, 0, 5, 1),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "callee"),
                    name: callee_name,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (6, 0, 8, 1),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
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
            process_start: 2,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
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

    /// Three-node chain: a(0) -[:Calls]-> b(1) -[:Calls]-> c(2)
    fn build_three_chain() -> Vec<u8> {
        let mut pool = StringPool::new();
        let na = pool.add("a");
        let nb = pool.add("b");
        let nc = pool.add("c");
        let fp = pool.add("src/x.ts");
        let r1 = pool.add("r1");
        let r2 = pool.add("r2");

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: fp,
                mtime: 0,
                content_hash: [0u8; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "a"),
                    name: na,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (0, 0, 1, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "b"),
                    name: nb,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (2, 0, 3, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "c"),
                    name: nc,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (4, 0, 5, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
            ],
            edges: vec![
                Edge {
                    source: 0,
                    target: 1,
                    rel_type: RelType::Calls,
                    confidence: 1.0,
                    reason: r1,
                },
                Edge {
                    source: 1,
                    target: 2,
                    rel_type: RelType::Calls,
                    confidence: 1.0,
                    reason: r2,
                },
            ],
            out_offsets: vec![0, 1, 2, 2],
            in_offsets: vec![0, 0, 1, 2],
            in_edge_idx: vec![0, 1],
            name_index: vec![],
            process_start: 3,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec()
    }

    fn with_three<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_three_chain();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    /// Four-node chain: a(0)->b(1)->c(2)->d(3) all :Calls
    fn build_four_chain() -> Vec<u8> {
        let mut pool = StringPool::new();
        let names = ["a", "b", "c", "d"];
        let nrefs: Vec<_> = names.iter().map(|n| pool.add(n)).collect();
        let fp = pool.add("src/x.ts");
        let reasons: Vec<_> = (0..3).map(|i| pool.add(&format!("r{i}"))).collect();

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: fp,
                mtime: 0,
                content_hash: [0u8; 8],
                category: FileCategory::Source,
            }],
            nodes: names
                .iter()
                .zip(nrefs.iter())
                .enumerate()
                .map(|(i, (name, &nref))| Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, name),
                    name: nref,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (i as u32 * 2, 0, i as u32 * 2 + 1, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                })
                .collect(),
            edges: vec![
                Edge {
                    source: 0,
                    target: 1,
                    rel_type: RelType::Calls,
                    confidence: 1.0,
                    reason: reasons[0],
                },
                Edge {
                    source: 1,
                    target: 2,
                    rel_type: RelType::Calls,
                    confidence: 1.0,
                    reason: reasons[1],
                },
                Edge {
                    source: 2,
                    target: 3,
                    rel_type: RelType::Calls,
                    confidence: 1.0,
                    reason: reasons[2],
                },
            ],
            out_offsets: vec![0, 1, 2, 3, 3],
            in_offsets: vec![0, 0, 1, 2, 3],
            in_edge_idx: vec![0, 1, 2],
            name_index: vec![],
            process_start: 4,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec()
    }

    fn with_four<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_four_chain();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    // -----------------------------------------------------------------------
    // Single-hop MATCH
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

    // -----------------------------------------------------------------------
    // Multi-hop chain (3 nodes)
    // -----------------------------------------------------------------------

    #[test]
    fn exec_three_hop_chain_returns_one_row() {
        with_three(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls]->(b:Function)-[:Calls]->(c:Function) RETURN a.name, b.name, c.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("a".into()));
            assert_eq!(r.rows[0][1], Value::Str("b".into()));
            assert_eq!(r.rows[0][2], Value::Str("c".into()));
        });
    }

    // -----------------------------------------------------------------------
    // Variable-length BFS (*min..max)
    // -----------------------------------------------------------------------

    #[test]
    fn exec_var_len_bfs_one_to_three() {
        // Chain: a->b->c->d. `*1..3` from a should reach b, c, d.
        with_four(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls*1..3]->(b:Function) WHERE a.name = 'a' RETURN b.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 3, "expected 3 rows, got {:?}", r.rows);
            let names: Vec<&str> = r
                .rows
                .iter()
                .map(|row| {
                    if let Value::Str(s) = &row[0] {
                        s.as_str()
                    } else {
                        ""
                    }
                })
                .collect();
            assert!(names.contains(&"b"), "missing b");
            assert!(names.contains(&"c"), "missing c");
            assert!(names.contains(&"d"), "missing d");
        });
    }

    #[test]
    fn exec_var_len_min_two_skips_direct_neighbour() {
        // `*2..3` from a should skip b, reach c and d.
        with_four(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls*2..3]->(b:Function) WHERE a.name = 'a' RETURN b.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2, "expected c and d, got {:?}", r.rows);
        });
    }

    // -----------------------------------------------------------------------
    // Bidirectional and reverse arrows
    // -----------------------------------------------------------------------

    #[test]
    fn exec_reverse_arrow() {
        // callee <-[:Calls]- caller  →  same edge, traversed in reverse
        with_two(|g| {
            let q =
                parse("MATCH (b:Function)<-[:Calls]-(a:Function) RETURN a.name, b.name").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("caller".into()));
            assert_eq!(r.rows[0][1], Value::Str("callee".into()));
        });
    }

    #[test]
    fn exec_undirected_finds_both_directions() {
        // undirected: same edge traversed out (caller→callee) and in (callee←caller)
        with_two(|g| {
            let q =
                parse("MATCH (a:Function)-[:Calls]-(b:Function) RETURN a.name, b.name").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2);
        });
    }

    // -----------------------------------------------------------------------
    // WHERE with edge props, IN, regex, CONTAINS
    // -----------------------------------------------------------------------

    #[test]
    fn exec_where_edge_confidence() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) WHERE r.confidence > 0.5 RETURN a.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
        });
    }

    #[test]
    fn exec_where_in_list() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls]->(b:Function) WHERE a.name IN ['caller', 'other'] RETURN b.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
        });
    }

    #[test]
    fn exec_where_regex() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls]->(b:Function) WHERE a.name =~ '.*aller.*' RETURN a.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
        });
    }

    #[test]
    fn exec_where_contains() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls]->(b:Function) WHERE b.name CONTAINS 'all' RETURN b.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
        });
    }

    #[test]
    fn exec_where_starts_with() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[:Calls]->(b:Function) WHERE a.name STARTS WITH 'cal' RETURN a.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
        });
    }

    #[test]
    fn exec_where_edge_reason() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) WHERE r.reason = 'ast-call' RETURN a.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
        });
    }

    // -----------------------------------------------------------------------
    // Scalar functions: type(r), id(n), labels(n) — must NOT be routed
    // through apply_aggregate (regression for FunCall-flagged-as-aggregate bug).
    // -----------------------------------------------------------------------

    #[test]
    fn exec_scalar_type_of_edge() {
        with_two(|g| {
            let q = parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN type(r)").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("Calls".into()));
        });
    }

    #[test]
    fn exec_scalar_id_of_node() {
        with_two(|g| {
            let q = parse("MATCH (a:Function) RETURN id(a)").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2);
            // First node's id is its index in graph.nodes.
            assert!(matches!(r.rows[0][0], Value::Int(_)));
        });
    }

    #[test]
    fn exec_scalar_labels_of_node() {
        with_two(|g| {
            let q = parse("MATCH (a:Function) WHERE a.name = 'caller' RETURN labels(a)").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            match &r.rows[0][0] {
                Value::List(xs) => {
                    assert_eq!(xs.len(), 1);
                    assert_eq!(xs[0], Value::Str("Function".into()));
                }
                v => panic!("expected labels list, got {v:?}"),
            }
        });
    }

    #[test]
    fn exec_scalar_mixed_with_aggregate() {
        // type(r) used as group key alongside count(*) aggregate.
        with_two(|g| {
            let q =
                parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN type(r), count(*) AS c")
                    .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("Calls".into()));
            assert_eq!(r.rows[0][1], Value::Int(1));
        });
    }

    // -----------------------------------------------------------------------
    // OPTIONAL MATCH left-join
    // -----------------------------------------------------------------------

    /// Single isolated node with no outgoing edges.
    fn build_lone_node() -> Vec<u8> {
        let mut pool = StringPool::new();
        let nm = pool.add("lone");
        let fp = pool.add("src/x.ts");

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: fp,
                mtime: 0,
                content_hash: [0u8; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![Node {
                uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "lone"),
                name: nm,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (0, 0, 1, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            }],
            edges: vec![],
            out_offsets: vec![0, 0],
            in_offsets: vec![0, 0],
            in_edge_idx: vec![],
            name_index: vec![],
            process_start: 1,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec()
    }

    fn with_lone<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_lone_node();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    #[test]
    fn exec_optional_match_returns_null_for_missing_hop() {
        // "lone" has no outgoing edges; OPTIONAL MATCH yields one row with b.name = null.
        with_lone(|g| {
            let q =
                parse("MATCH (a:Function) OPTIONAL MATCH (a)-[:Calls]->(b) RETURN a.name, b.name")
                    .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                1,
                "expected 1 row from OPTIONAL MATCH left-join"
            );
            assert_eq!(r.rows[0][0], Value::Str("lone".into()));
            assert_eq!(r.rows[0][1], Value::Null);
        });
    }

    // -----------------------------------------------------------------------
    // Aggregation fixture: fan(0)->leaf_a(1), fan(0)->leaf_b(2)
    // fan calls leaf_a (conf=0.8) and leaf_b (conf=0.6).
    // -----------------------------------------------------------------------

    fn build_fan() -> Vec<u8> {
        let mut pool = StringPool::new();
        let n_fan = pool.add("fan");
        let n_leaf_a = pool.add("leaf_a");
        let n_leaf_b = pool.add("leaf_b");
        let fp = pool.add("src/x.ts");
        let r1 = pool.add("r1");
        let r2 = pool.add("r2");

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: fp,
                mtime: 0,
                content_hash: [0u8; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "fan"),
                    name: n_fan,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (0, 0, 1, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "leaf_a"),
                    name: n_leaf_a,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (2, 0, 3, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "leaf_b"),
                    name: n_leaf_b,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (4, 0, 5, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
            ],
            edges: vec![
                Edge {
                    source: 0,
                    target: 1,
                    rel_type: RelType::Calls,
                    confidence: 0.8,
                    reason: r1,
                },
                Edge {
                    source: 0,
                    target: 2,
                    rel_type: RelType::Calls,
                    confidence: 0.6,
                    reason: r2,
                },
            ],
            // Node 0 has edges 0..2 out; nodes 1,2 have no outgoing.
            out_offsets: vec![0, 2, 2, 2],
            // Node 1 has edge 0 incoming; node 2 has edge 1 incoming.
            in_offsets: vec![0, 0, 1, 2],
            in_edge_idx: vec![0, 1],
            name_index: vec![],
            process_start: 3,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec()
    }

    fn with_fan<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_fan();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    // Aggregation tests

    #[test]
    fn exec_count_star() {
        // fan graph: MATCH (a:Function) RETURN COUNT(*) → 3 nodes total
        with_fan(|g| {
            let q = parse("MATCH (a:Function) RETURN COUNT(*)").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "expected 1 aggregated row");
            assert_eq!(r.rows[0][0], Value::Int(3));
        });
    }

    #[test]
    fn exec_count_grouped() {
        // fan->leaf_a, fan->leaf_b: grouping by a.name gives fan→2, leaf_a→0, leaf_b→0.
        // Use MATCH (a)-[:Calls]->(b) RETURN a.name, COUNT(*): 2 rows (both under fan)
        with_fan(|g| {
            let q =
                parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN a.name, COUNT(*)").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            // fan calls both leaf_a and leaf_b → 2 bindings, all with a.name="fan"
            // so 1 group: fan → COUNT=2
            assert_eq!(r.rows.len(), 1, "expected 1 group: fan→2, got {:?}", r.rows);
            assert_eq!(r.rows[0][0], Value::Str("fan".into()));
            assert_eq!(r.rows[0][1], Value::Int(2));
        });
    }

    #[test]
    fn exec_count_distinct() {
        // MATCH (a)-[:Calls]->(b) RETURN COUNT(DISTINCT b.name)
        // two different targets leaf_a, leaf_b → 2 distinct
        with_fan(|g| {
            let q =
                parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN COUNT(DISTINCT b.name)")
                    .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Int(2));
        });
    }

    #[test]
    fn exec_count_edge_var_returns_actual_count() {
        // Pre-fix bug: `COUNT(r)` where `r` is an edge variable evaluated
        // every binding's `r` to Null (the `Var` arm of `eval_expr` only
        // looked at `computed` + `node_vars`, falling through on edge
        // vars). Aggregate's null-skip then yielded 0 even when the
        // pattern produced matching bindings.
        //
        // Fan graph: fan->leaf_a, fan->leaf_b → 2 Calls edges. Expected
        // COUNT(r) = 2, matching COUNT(*) on the same pattern.
        with_fan(|g| {
            let q = parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN COUNT(r)").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(
                r.rows[0][0],
                Value::Int(2),
                "COUNT(r) on edge var must count matched edges, got {:?}",
                r.rows[0][0]
            );
        });
    }

    #[test]
    fn exec_count_distinct_edge_var() {
        // DISTINCT on edge var: each matched edge is structurally
        // distinct (different src/tgt/reason), so two edges still
        // count as two.
        with_fan(|g| {
            let q = parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN COUNT(DISTINCT r)")
                .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Int(2));
        });
    }

    #[test]
    fn exec_sum_min_max_avg() {
        // fan->leaf_a conf=0.8, fan->leaf_b conf=0.6
        with_fan(|g| {
            let q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) RETURN SUM(r.confidence), MIN(r.confidence), MAX(r.confidence), AVG(r.confidence)",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            // SUM ≈ 1.4 (f32→f64 precision: tolerance 1e-6)
            assert!(matches!(r.rows[0][0], Value::Float(f) if (f - 1.4).abs() < 1e-6));
            // MIN ≈ 0.6
            assert!(matches!(r.rows[0][1], Value::Float(f) if (f - 0.6).abs() < 1e-6));
            // MAX ≈ 0.8
            assert!(matches!(r.rows[0][2], Value::Float(f) if (f - 0.8).abs() < 1e-6));
            // AVG ≈ 0.7
            assert!(matches!(r.rows[0][3], Value::Float(f) if (f - 0.7).abs() < 1e-6));
        });
    }

    #[test]
    fn exec_collect_list() {
        // COLLECT(b.name) → list of leaf names
        with_fan(|g| {
            let q =
                parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN COLLECT(b.name)").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            let list = match &r.rows[0][0] {
                Value::List(v) => v.clone(),
                other => panic!("expected List, got {other:?}"),
            };
            assert_eq!(list.len(), 2);
            assert!(list.contains(&Value::Str("leaf_a".into())));
            assert!(list.contains(&Value::Str("leaf_b".into())));
        });
    }

    #[test]
    fn exec_with_aggregate_then_filter() {
        // WITH a, COUNT(*) AS n WHERE n > 0 RETURN a.name, n
        // fan calls 2 targets; leaf_a and leaf_b call nothing.
        // After WITH aggregation: fan→n=2, leaf_a→n=0, leaf_b→n=0.
        // WHERE n > 0 keeps only fan row.
        with_fan(|g| {
            let q = parse(
                "MATCH (a:Function) OPTIONAL MATCH (a)-[:Calls]->(b) WITH a, COUNT(b) AS n WHERE n > 0 RETURN a.name, n",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                1,
                "only fan should pass WHERE n > 0, got {:?}",
                r.rows
            );
            assert_eq!(r.rows[0][0], Value::Str("fan".into()));
            assert_eq!(r.rows[0][1], Value::Int(2));
        });
    }

    #[test]
    fn exec_with_plain_rebinding() {
        // WITH a.name AS nm RETURN nm
        with_fan(|g| {
            let q = parse("MATCH (a:Function)-[:Calls]->(b:Function) WITH a.name AS nm RETURN nm")
                .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            // Two hops from fan: both have a.name="fan"
            assert_eq!(r.rows.len(), 2);
            assert_eq!(r.columns, vec!["nm"]);
            assert!(r.rows.iter().all(|row| row[0] == Value::Str("fan".into())));
        });
    }

    #[test]
    fn exec_optional_match_still_returns_when_present() {
        // two-node fixture: OPTIONAL MATCH should behave like MATCH when edge exists.
        with_two(|g| {
            let q =
                parse("MATCH (a:Function) OPTIONAL MATCH (a)-[:Calls]->(b) RETURN a.name, b.name")
                    .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            // caller has callee; callee has no outgoing → 2 rows
            assert_eq!(r.rows.len(), 2);
            let caller_row = r
                .rows
                .iter()
                .find(|row| row[0] == Value::Str("caller".into()))
                .unwrap();
            assert_eq!(caller_row[1], Value::Str("callee".into()));
            let callee_row = r
                .rows
                .iter()
                .find(|row| row[0] == Value::Str("callee".into()))
                .unwrap();
            assert_eq!(callee_row[1], Value::Null);
        });
    }

    // -----------------------------------------------------------------------
    // RETURN auto-expand bare node/edge vars
    // -----------------------------------------------------------------------

    #[test]
    fn exec_return_auto_expand_node() {
        // RETURN a for a node-bound var → 3 columns: a.name, a.kind, a.filePath
        with_two(|g| {
            let q = parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN a").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.columns, vec!["a.name", "a.kind", "a.filePath"]);
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0].len(), 3, "expected 3 values per row");
            assert_eq!(r.rows[0][0], Value::Str("caller".into()));
            assert_eq!(r.rows[0][1], Value::Str("Function".into()));
            assert_eq!(r.rows[0][2], Value::Str("src/x.ts".into()));
        });
    }

    #[test]
    fn exec_return_auto_expand_edge() {
        // RETURN r for an edge-bound var → 3 columns: r.rel_type, r.confidence, r.reason
        with_two(|g| {
            let q = parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN r").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.columns, vec!["r.rel_type", "r.confidence", "r.reason"]);
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0].len(), 3);
            assert_eq!(r.rows[0][0], Value::Str("Calls".into()));
            assert!(matches!(r.rows[0][1], Value::Float(f) if (f - 1.0).abs() < 1e-6));
            assert_eq!(r.rows[0][2], Value::Str("ast-call".into()));
        });
    }

    // -----------------------------------------------------------------------
    // DISTINCT + ORDER BY + SKIP + LIMIT
    // -----------------------------------------------------------------------

    #[test]
    fn exec_order_by_asc_desc() {
        // fan graph: 3 nodes. Sort by name asc → a_leaf_a, fan, leaf_b order?
        // Actually nodes are fan(0), leaf_a(1), leaf_b(2). Sort asc → fan < leaf_a < leaf_b
        with_fan(|g| {
            let q = parse("MATCH (a:Function) RETURN a.name ORDER BY a.name ASC").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 3);
            let names: Vec<&str> = r
                .rows
                .iter()
                .map(|row| {
                    if let Value::Str(s) = &row[0] {
                        s.as_str()
                    } else {
                        ""
                    }
                })
                .collect();
            // Lexicographic: fan < leaf_a < leaf_b
            assert_eq!(names, vec!["fan", "leaf_a", "leaf_b"]);
        });
    }

    #[test]
    fn exec_order_by_desc() {
        with_fan(|g| {
            let q = parse("MATCH (a:Function) RETURN a.name ORDER BY a.name DESC").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            let names: Vec<&str> = r
                .rows
                .iter()
                .map(|row| {
                    if let Value::Str(s) = &row[0] {
                        s.as_str()
                    } else {
                        ""
                    }
                })
                .collect();
            assert_eq!(names, vec!["leaf_b", "leaf_a", "fan"]);
        });
    }

    #[test]
    fn exec_distinct() {
        // fan calls leaf_a and leaf_b; both hops have a.name="fan".
        // RETURN DISTINCT a.name → 1 unique row.
        with_fan(|g| {
            let q =
                parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN DISTINCT a.name").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "expected 1 distinct row, got {:?}", r.rows);
            assert_eq!(r.rows[0][0], Value::Str("fan".into()));
        });
    }

    #[test]
    fn exec_skip_and_limit() {
        // 3 nodes sorted by name asc: fan, leaf_a, leaf_b. SKIP 1 LIMIT 1 → leaf_a.
        with_fan(|g| {
            let q = parse("MATCH (a:Function) RETURN a.name ORDER BY a.name ASC SKIP 1 LIMIT 1")
                .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "expected 1 row after skip+limit");
            assert_eq!(r.rows[0][0], Value::Str("leaf_a".into()));
        });
    }

    // -----------------------------------------------------------------------
    // UNION / UNION ALL
    // -----------------------------------------------------------------------

    /// Build a graph with one Function node and one Method node.
    fn build_func_and_method() -> Vec<u8> {
        let mut pool = StringPool::new();
        let n_func = pool.add("my_func");
        let n_meth = pool.add("my_method");
        let fp = pool.add("src/x.ts");

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: fp,
                mtime: 0,
                content_hash: [0u8; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "my_func"),
                    name: n_func,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (0, 0, 1, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Method, "src/x.ts", None, "my_method"),
                    name: n_meth,
                    file_idx: 0,
                    kind: NodeKind::Method,
                    span: (2, 0, 3, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
            ],
            edges: vec![],
            out_offsets: vec![0, 0, 0],
            in_offsets: vec![0, 0, 0],
            in_edge_idx: vec![],
            name_index: vec![],
            process_start: 2,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec()
    }

    fn with_func_and_method<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_func_and_method();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    #[test]
    fn exec_union_concat() {
        // UNION concatenates results from two sub-queries.
        with_func_and_method(|g| {
            let q = parse("MATCH (a:Function) RETURN a.name UNION MATCH (b:Method) RETURN b.name")
                .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            // 1 Function + 1 Method = 2 rows (distinct by default).
            assert_eq!(r.columns, vec!["a.name"], "left-side column names kept");
            let names: Vec<&str> = r
                .rows
                .iter()
                .map(|row| {
                    if let Value::Str(s) = &row[0] {
                        s.as_str()
                    } else {
                        ""
                    }
                })
                .collect();
            assert!(names.contains(&"my_func"), "missing my_func");
            assert!(names.contains(&"my_method"), "missing my_method");
            assert_eq!(r.rows.len(), 2);
        });
    }

    #[test]
    fn exec_union_all_keeps_dupes() {
        // UNION ALL keeps duplicates; matching all :Function nodes gives 1 from left, 1 from right.
        // Actually in this fixture Function=my_func, Method=my_method.
        // Use fan fixture where there are 3 Function nodes.
        with_fan(|g| {
            // Both sides match all Function nodes → 6 rows with UNION ALL.
            let q = parse(
                "MATCH (a:Function) RETURN a.name UNION ALL MATCH (b:Function) RETURN b.name",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 6, "UNION ALL keeps duplicates: 3+3");
        });
    }

    #[test]
    fn exec_union_dedupes_without_all() {
        // UNION (no ALL) deduplicates.
        with_fan(|g| {
            let q =
                parse("MATCH (a:Function) RETURN a.name UNION MATCH (b:Function) RETURN b.name")
                    .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 3, "UNION deduplicates: 3 unique names");
        });
    }

    // -----------------------------------------------------------------------
    // .content projection via lazy file read
    // -----------------------------------------------------------------------

    #[test]
    fn exec_content_projection() {
        use std::io::Write;

        // Write a temp source file.
        let dir = tempfile::tempdir().expect("temp dir");
        let src_path = dir.path().join("hello.ts");
        {
            let mut f = std::fs::File::create(&src_path).unwrap();
            // Line 0: "function hello() {"
            // Line 1: "  return 42;"
            // Line 2: "}"
            write!(f, "function hello() {{\n  return 42;\n}}").unwrap();
        }
        let rel_path = "hello.ts";

        let mut pool = StringPool::new();
        let n_name = pool.add("hello");
        let fp = pool.add(rel_path);

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: fp,
                mtime: 0,
                content_hash: [0u8; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![Node {
                uid: crate::uid::compute(NodeKind::Function, "hello.ts", None, "hello"),
                name: n_name,
                file_idx: 0,
                kind: NodeKind::Function,
                // span: start_row=0, start_col=0, end_row=2, end_col=1
                span: (0, 0, 2, 1),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            }],
            edges: vec![],
            out_offsets: vec![0, 0],
            in_offsets: vec![0, 0],
            in_edge_idx: vec![],
            name_index: vec![],
            process_start: 1,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();

        let q = parse("MATCH (a:Function) RETURN a.content").unwrap();
        let result = execute(&q, archived, dir.path()).unwrap();

        assert_eq!(result.columns, vec!["a.content"]);
        assert_eq!(result.rows.len(), 1);
        let content = match &result.rows[0][0] {
            Value::Str(s) => s.clone(),
            other => panic!("expected Str, got {other:?}"),
        };
        // span (0,0,2,1) covers "function hello() {\n  return 42;\n}"
        assert!(content.contains("function hello"), "content: {content:?}");
        assert!(content.contains("return 42"), "content: {content:?}");
    }

    // -----------------------------------------------------------------------
    // FunctionMeta property whitelist tests
    // Fixture: 6 nodes —
    //   0: "sync_fn"   Function, no FunctionMeta (sparse-record absent)
    //   1: "async_fn"  Function, is_async=true
    //   2: "test_fn"   Function, is_test=true
    //   3: "both_fn"   Function, is_test=true + is_async=true
    //   4: "override_method" Method, decorators=["@Override"], private
    //   5: "py_route"  Function, decorators=["app.get"], no flags
    // -----------------------------------------------------------------------

    fn build_function_meta_graph() -> Vec<u8> {
        use crate::graph::{FunctionMeta, NodeKind};

        let mut pool = StringPool::new();
        let fp = pool.add("src/x.ts");

        let n_sync = pool.add("sync_fn");
        let n_async = pool.add("async_fn");
        let n_test = pool.add("test_fn");
        let n_both = pool.add("both_fn");
        let n_override = pool.add("override_method");
        let n_route = pool.add("py_route");

        let dec_override = pool.add("@Override");
        let dec_appget = pool.add("app.get");

        // visibility=private (2) encodes into bits 6-8: 2 << 6 = 0x80
        const PRIVATE_VISIBILITY: u16 = 2 << 6;

        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: fp,
                mtime: 0,
                content_hash: [0u8; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "sync_fn"),
                    name: n_sync,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (0, 0, 1, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "async_fn"),
                    name: n_async,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (2, 0, 3, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "test_fn"),
                    name: n_test,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (4, 0, 5, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "both_fn"),
                    name: n_both,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (6, 0, 7, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Method, "src/x.ts", None, "override_method"),
                    name: n_override,
                    file_idx: 0,
                    kind: NodeKind::Method,
                    span: (8, 0, 9, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
                Node {
                    uid: crate::uid::compute(NodeKind::Function, "src/x.ts", None, "py_route"),
                    name: n_route,
                    file_idx: 0,
                    kind: NodeKind::Function,
                    span: (10, 0, 11, 0),
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                },
            ],
            edges: vec![],
            out_offsets: vec![0, 0, 0, 0, 0, 0, 0],
            in_offsets: vec![0, 0, 0, 0, 0, 0, 0],
            in_edge_idx: vec![],
            name_index: vec![],
            process_start: 6,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            // node_idx 0 intentionally absent (tests sparse-record absence for sync_fn)
            function_metas: vec![
                FunctionMeta {
                    node_idx: 1,
                    flags: FunctionMeta::FLAG_ASYNC,
                    params: vec![],
                    return_type: StrRef::default(),
                    decorators: vec![],
                },
                FunctionMeta {
                    node_idx: 2,
                    flags: FunctionMeta::FLAG_TEST,
                    params: vec![],
                    return_type: StrRef::default(),
                    decorators: vec![],
                },
                FunctionMeta {
                    node_idx: 3,
                    flags: FunctionMeta::FLAG_TEST | FunctionMeta::FLAG_ASYNC,
                    params: vec![],
                    return_type: StrRef::default(),
                    decorators: vec![],
                },
                FunctionMeta {
                    node_idx: 4,
                    flags: PRIVATE_VISIBILITY,
                    params: vec![],
                    return_type: StrRef::default(),
                    decorators: vec![dec_override],
                },
                FunctionMeta {
                    node_idx: 5,
                    flags: 0,
                    params: vec![],
                    return_type: StrRef::default(),
                    decorators: vec![dec_appget],
                },
            ],
            kind_offsets: vec![],
            kind_node_idx: vec![],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec()
    }

    fn with_fm<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_function_meta_graph();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    // a) async-only filter returns async functions, excludes sync
    #[test]
    fn fm_is_async_filter_returns_async_excludes_sync() {
        with_fm(|g| {
            let q =
                parse("MATCH (f:Function|Method) WHERE f.is_async = true RETURN f.name").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            let names: Vec<_> = r
                .rows
                .iter()
                .map(|row| match &row[0] {
                    Value::Str(s) => s.clone(),
                    other => panic!("expected Str, got {other:?}"),
                })
                .collect();
            assert!(names.contains(&"async_fn".to_string()), "async_fn missing");
            assert!(names.contains(&"both_fn".to_string()), "both_fn missing");
            assert!(
                !names.contains(&"sync_fn".to_string()),
                "sync_fn must be excluded"
            );
        });
    }

    // b) is_test filter with mixed test/non-test nodes
    #[test]
    fn fm_is_test_filter_mixed_nodes() {
        with_fm(|g| {
            let q =
                parse("MATCH (f:Function|Method) WHERE f.is_test = true RETURN f.name").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            let names: Vec<_> = r
                .rows
                .iter()
                .map(|row| match &row[0] {
                    Value::Str(s) => s.clone(),
                    other => panic!("expected Str, got {other:?}"),
                })
                .collect();
            assert!(names.contains(&"test_fn".to_string()), "test_fn missing");
            assert!(names.contains(&"both_fn".to_string()), "both_fn missing");
            assert!(
                !names.contains(&"async_fn".to_string()),
                "async_fn must be excluded"
            );
            assert!(
                !names.contains(&"sync_fn".to_string()),
                "sync_fn must be excluded"
            );
        });
    }

    // c) decorators IN-membership: Java @Override queryable without @
    #[test]
    fn fm_decorator_in_membership() {
        with_fm(|g| {
            let q =
                parse("MATCH (m:Function|Method) WHERE 'Override' IN m.decorators RETURN m.name")
                    .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "expected exactly override_method");
            assert_eq!(r.rows[0][0], Value::Str("override_method".into()));
        });
    }

    // d) decorator @ normalization: Python app.get and Java @Override both queryable without @
    #[test]
    fn fm_decorator_at_normalization() {
        with_fm(|g| {
            // app.get has no @ — queryable as-is
            let q_py =
                parse("MATCH (f:Function|Method) WHERE 'app.get' IN f.decorators RETURN f.name")
                    .unwrap();
            let r_py = execute(&q_py, g, Path::new(".")).unwrap();
            assert_eq!(r_py.rows.len(), 1, "expected py_route for app.get");
            assert_eq!(r_py.rows[0][0], Value::Str("py_route".into()));

            // @Override stored as "@Override" but normalized to "Override"
            let q_java =
                parse("MATCH (m:Function|Method) WHERE 'Override' IN m.decorators RETURN m.name")
                    .unwrap();
            let r_java = execute(&q_java, g, Path::new(".")).unwrap();
            assert_eq!(
                r_java.rows.len(),
                1,
                "expected override_method for Override"
            );
            assert_eq!(r_java.rows[0][0], Value::Str("override_method".into()));

            // raw "@Override" with leading @ should NOT match after normalization
            let q_raw =
                parse("MATCH (m:Function|Method) WHERE '@Override' IN m.decorators RETURN m.name")
                    .unwrap();
            let r_raw = execute(&q_raw, g, Path::new(".")).unwrap();
            assert!(r_raw.rows.is_empty(), "@Override with @ should not match");
        });
    }

    // e) visibility = 2 returns only private nodes
    #[test]
    fn fm_visibility_private_filter() {
        with_fm(|g| {
            let q =
                parse("MATCH (f:Function|Method) WHERE f.visibility = 2 RETURN f.name").unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "expected exactly override_method");
            assert_eq!(r.rows[0][0], Value::Str("override_method".into()));
        });
    }

    // f) node with NO FunctionMeta: is_async returns false (not Null), decorators returns empty list
    #[test]
    fn fm_absent_record_returns_safe_defaults() {
        with_fm(|g| {
            // sync_fn has no FunctionMeta record
            let q = parse(
                "MATCH (f:Function) WHERE f.name = 'sync_fn' RETURN f.is_async, f.decorators",
            )
            .unwrap();
            let r = execute(&q, g, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(
                r.rows[0][0],
                Value::Bool(false),
                "is_async must be false, not Null"
            );
            assert_eq!(
                r.rows[0][1],
                Value::List(vec![]),
                "decorators must be empty list, not Null"
            );
        });
    }
}
