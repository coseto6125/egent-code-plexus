use crate::cypher::ast::*;
use crate::cypher::error::CypherError;
use crate::cypher::value::{QueryResult, Value};
use crate::graph::{ArchivedZeroCopyGraph, RelType};
use crate::session::{MergedGraph, MergedNode, OverlayView};
use compact_str::CompactString;
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Small-N variable lookup. Replaces `HashMap<String, u32>` on the
/// `Binding` fields cloned once per matched node in `exec_pattern`'s
/// frontier expansion. Typical cypher queries bind <=4 vars, so the
/// SmallVec inline storage never spills — clone is a fixed-size memcpy
/// instead of a HashMap bucket allocation. `CompactString` keys inline
/// up to 24 bytes (every realistic var name) so the per-key clone is
/// also alloc-free. Linear scan over <=4 entries beats HashMap at this
/// size because there is no hashing cost.
#[derive(Debug, Clone, Default)]
struct VarMap {
    entries: SmallVec<[(CompactString, u32); 4]>,
}

impl VarMap {
    #[inline]
    fn get(&self, key: &str) -> Option<&u32> {
        self.entries
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| v)
    }

    #[inline]
    fn contains_key(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k.as_str() == key)
    }

    #[inline]
    fn insert(&mut self, key: &str, value: u32) {
        for (k, v) in &mut self.entries {
            if k.as_str() == key {
                *v = value;
                return;
            }
        }
        self.entries.push((CompactString::from(key), value));
    }
}

/// One row of intermediate bindings during pattern matching.
#[derive(Debug, Clone, Default)]
struct Binding {
    /// var_name -> node index into `graph.nodes`
    node_vars: VarMap,
    /// var_name -> edge index into `graph.edges`
    edge_vars: VarMap,
    /// Values computed by a prior WITH clause. Checked before node_vars/edge_vars
    /// in prop_value and project_item. Stays as `HashMap` because it carries
    /// `Value` (non-Copy, larger) and is only populated after WITH — frontier
    /// expansion clones the empty default (cheap 56-byte memcpy).
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
        if file_idx == crate::graph::SYNTHETIC_FILE_IDX {
            return None;
        }
        self.files
            .entry(file_idx)
            .or_insert_with(|| {
                graph.files.get(file_idx as usize).and_then(|f| {
                    let rel = f.path.resolve(&graph.string_pool);
                    std::fs::read_to_string(self.repo_root.join(rel)).ok()
                })
            })
            .as_deref()
    }
}

pub fn execute(
    query: &Query,
    graph: &ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    repo_root: &Path,
) -> Result<QueryResult, CypherError> {
    let gv = MergedGraph::new(graph, view);
    let mut cache = ContentCache::new(repo_root.to_path_buf());
    let rewritten = pushdown_where(query);
    execute_inner(rewritten.as_ref().unwrap_or(query), gv, &mut cache)
}

/// Move `var.prop = literal` conjuncts from the top-level WHERE into the node
/// patterns that introduce those vars, so they filter during the scan/walk
/// instead of after full materialisation (`MATCH (a)-[:Calls]->(b) WHERE
/// b.name = 'x'` otherwise materialises every Calls binding first).
///
/// A conjunct moves only when ALL of:
/// - it is a top-level AND term of shape `var.prop = lit` (either side),
/// - the prop is not `content` (node_matches excludes it: needs a file read)
///   and not `uid` against a string literal (that shape has a dedicated eval
///   diagnostic worth keeping),
/// - the var's first appearance across the ordered MATCH clauses is in a
///   non-OPTIONAL clause — pushing into an OPTIONAL pattern would flip
///   "row kept with null, then WHERE-filtered" into "row kept".
///
/// Returns the rewritten query, or `None` when nothing can move (the caller
/// then runs the original, clone-free).
fn pushdown_where(q: &Query) -> Option<Query> {
    // var -> introduced by an OPTIONAL clause? First appearance wins.
    let mut first_optional: Vec<(&str, bool)> = Vec::new();
    for mc in &q.matches {
        for pat in &mc.patterns {
            for n in &pat.nodes {
                if let Some(v) = n.var.as_deref() {
                    if !first_optional.iter().any(|(k, _)| *k == v) {
                        first_optional.push((v, mc.optional));
                    }
                }
            }
        }
    }
    let pushable = |e: &Expr| -> Option<(String, String, Literal)> {
        let Expr::BinOp(Op::Eq, l, r) = e else {
            return None;
        };
        let (v, p, lit) = match (&**l, &**r) {
            (Expr::Prop(v, p), Expr::Lit(lit)) | (Expr::Lit(lit), Expr::Prop(v, p)) => (v, p, lit),
            _ => return None,
        };
        if p == "content" || (p == "uid" && matches!(lit, Literal::Str(_))) {
            return None;
        }
        match first_optional.iter().find(|(k, _)| k == v) {
            Some((_, false)) => Some((v.clone(), p.clone(), lit.clone())),
            _ => None,
        }
    };

    // Borrow-scan first: the common no-move query must not clone anything.
    let mut moved: Vec<(String, String, Literal)> = Vec::new();
    let mut residual: Vec<&Expr> = Vec::new();
    if let Some(w) = &q.where_ {
        let mut conjuncts = Vec::new();
        split_and(w, &mut conjuncts);
        for c in conjuncts {
            match pushable(c) {
                Some(t) => moved.push(t),
                None => residual.push(c),
            }
        }
    }

    let union_rw = q.union.as_deref().and_then(pushdown_where);
    if moved.is_empty() && union_rw.is_none() {
        return None;
    }

    let mut rw = q.clone();
    for (v, p, lit) in moved {
        let mut payload = Some((p, lit));
        // The first non-OPTIONAL occurrence introduces the binding — filtering
        // there prunes earliest; later occurrences agree via the binding pin.
        'place: for mc in rw.matches.iter_mut().filter(|m| !m.optional) {
            for pat in &mut mc.patterns {
                for n in &mut pat.nodes {
                    if n.var.as_deref() == Some(v.as_str()) {
                        n.props.push(payload.take().expect("placed once"));
                        break 'place;
                    }
                }
            }
        }
    }
    rw.where_ = residual
        .into_iter()
        .cloned()
        .reduce(|l, r| Expr::BinOp(Op::And, Box::new(l), Box::new(r)));
    if let Some(u) = union_rw {
        rw.union = Some(Box::new(u));
    }
    Some(rw)
}

fn split_and<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::BinOp(Op::And, l, r) = e {
        split_and(l, out);
        split_and(r, out);
    } else {
        out.push(e);
    }
}

fn execute_inner(
    query: &Query,
    graph: MergedGraph<'_>,
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

        // Identify aggregate positions once so the per-row loop avoids re-scanning.
        // Each entry: (expanded_items index, is_count_star, arg expr, kind, distinct).
        let agg_positions: Vec<(usize, bool, Option<&Expr>, AggregateKind, bool)> = expanded_items
            .iter()
            .enumerate()
            .filter_map(|(i, (_, e))| {
                if let ReturnExpr::FunCall {
                    name,
                    distinct,
                    args,
                } = e
                {
                    let kind = AggregateKind::parse(name)?;
                    let is_cs = matches!(args.as_slice(), [Expr::Lit(Literal::Null)]);
                    let arg = if is_cs { None } else { args.first() };
                    return Some((i, is_cs, arg, kind, *distinct));
                }
                None
            })
            .collect();

        // Fast path: ungrouped COUNT(*) is just the binding count — no per-row work.
        if group_items.is_empty() && agg_positions.len() == 1 && agg_positions[0].1
        // is_count_star
        {
            columns = expanded_items.iter().map(|(col, _)| col.clone()).collect();
            rows.push(vec![Value::Int(bindings.len() as i64)]);
        } else {
            // Build column names.
            for (col, _) in &expanded_items {
                columns.push(col.clone());
            }

            // Groups keyed by serialized group-key string.
            // Value: (key_vals, Vec<Accumulator> — one per agg_position slot).
            let mut groups: Vec<(Vec<Value>, Vec<Accumulator>)> = Vec::new();
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
                let slot = *key_index.entry(key_str).or_insert_with(|| {
                    let accums = agg_positions
                        .iter()
                        .map(|(_, _, _, kind, distinct)| Accumulator::new(*kind, *distinct))
                        .collect();
                    groups.push((key_vals.clone(), accums));
                    groups.len() - 1
                });
                let accums = &mut groups[slot].1;
                for (ai, (_, is_cs, arg_expr, _, _)) in agg_positions.iter().enumerate() {
                    let v = if *is_cs {
                        Value::Null
                    } else {
                        eval_expr(arg_expr.unwrap(), b, graph, cache)?
                    };
                    accums[ai].feed(v, *is_cs);
                }
            }

            // If no bindings at all and no group keys: emit one zero-row.
            if groups.is_empty() && group_items.is_empty() {
                let accums = agg_positions
                    .iter()
                    .map(|(_, _, _, kind, distinct)| Accumulator::new(*kind, *distinct))
                    .collect();
                groups.push((vec![], accums));
            }

            for (key_vals, accums) in groups {
                let mut row = Vec::with_capacity(expanded_items.len());
                let mut key_iter = key_vals.into_iter();
                let mut agg_iter = accums.into_iter();
                for (_, expr) in &expanded_items {
                    if let ReturnExpr::FunCall { name, .. } = expr {
                        if is_aggregate_fn(name) {
                            row.push(agg_iter.next().unwrap().finalize());
                        } else {
                            row.push(key_iter.next().unwrap_or(Value::Null));
                        }
                    } else {
                        row.push(key_iter.next().unwrap_or(Value::Null));
                    }
                }
                rows.push(row);
            }
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

    // Width invariant — every row must carry exactly one value per
    // projected column. Downstream consumers (e.g. `cypher::build_payload`
    // in the CLI) collapse single-column rows to scalars and fall back to
    // null on empty rows; a violation here would silently surface as a
    // legitimate null result. Caught in debug builds before it leaves the
    // executor.
    debug_assert!(
        rows.iter().all(|row| row.len() == columns.len()),
        "cypher executor invariant: every row must have row.len() == columns.len() (expected {})",
        columns.len()
    );

    Ok(QueryResult { columns, rows })
}

fn dedup_rows(rows: &mut Vec<Vec<Value>>) {
    let mut seen = HashSet::new();
    let mut key = Vec::new();
    rows.retain(|row| {
        key.clear();
        for v in row {
            v.write_dedup_key(&mut key);
        }
        seen.insert(key.clone())
    });
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

/// How a bound `Var` collapses when a `ReturnExpr::Var` is evaluated. The two
/// callers only ever disagree on this one axis — `Prop`/`Star`/`FunCall` are
/// identical either way — so it is the sole parameter distinguishing them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VarCollapse {
    /// Node/edge vars resolve to their display name (`Value::Str`) — used by
    /// the plain RETURN projection path, matching legacy scalar semantics.
    ToStr,
    /// Node/edge vars resolve to `Value::NodeRef`/`Value::EdgeRef`, preserving
    /// identity — used by WITH group-key computation so `a.name` still
    /// resolves after aggregation clears `node_vars`.
    ToRef,
}

/// Evaluate a ReturnExpr against a binding. Single dispatch point shared by
/// the plain RETURN projection path and WITH group-key computation; `collapse`
/// selects the one axis where they differ (see `VarCollapse`).
fn eval_return_expr_with(
    expr: &ReturnExpr,
    b: &Binding,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
    collapse: VarCollapse,
) -> Value {
    match expr {
        ReturnExpr::Var(var) => {
            if let Some(v) = b.computed.get(var) {
                return v.clone();
            }
            if let Some(&idx) = b.node_vars.get(var) {
                let Some(m) = graph.node(idx) else {
                    return Value::Null;
                };
                return match collapse {
                    VarCollapse::ToStr => Value::Str(m.name(&graph).into()),
                    VarCollapse::ToRef => Value::NodeRef {
                        idx,
                        name: m.name(&graph).into(),
                        kind: m.kind().as_str().into(),
                        file_path: m.file_path(&graph).unwrap_or("").to_string(),
                    },
                };
            }
            if collapse == VarCollapse::ToRef {
                if let Some(&eidx) = b.edge_vars.get(var) {
                    if let Some(e) = graph.overlay_edge(eidx) {
                        return Value::EdgeRef {
                            src: e.source,
                            tgt: e.target,
                            rel_type: e.rel_type,
                            confidence: e.confidence,
                            reason: crate::session::OVERLAY_EDGE_REASON.to_string(),
                        };
                    }
                    let e = &graph.edges[eidx as usize];
                    let rt = crate::graph::RelType::from(&e.rel_type);
                    return Value::EdgeRef {
                        src: e.source.to_native(),
                        tgt: e.target.to_native(),
                        rel_type: rt,
                        confidence: e.confidence.to_native(),
                        reason: e.reason.resolve(&graph.string_pool).to_string(),
                    };
                }
            }
            Value::Null
        }
        ReturnExpr::Prop(var, prop) => prop_value(var, prop, b, graph, cache),
        ReturnExpr::Star => Value::Null,
        ReturnExpr::FunCall { name, args, .. } => eval_scalar_funcall(name, args, b, graph),
    }
}

/// Evaluate a ReturnExpr directly against a binding (used in the non-agg projection path).
/// Aggregate FunCalls reach this path only when the caller already verified
/// there is no aggregate in the projection — so treating them as scalar is a
/// safe no-op (returns Null via `eval_scalar_funcall`'s unknown-name fallback).
fn eval_return_expr(
    expr: &ReturnExpr,
    b: &Binding,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<Value, CypherError> {
    Ok(eval_return_expr_with(
        expr,
        b,
        graph,
        cache,
        VarCollapse::ToStr,
    ))
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
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Value {
    eval_return_expr_with(&item.expr, b, graph, cache, VarCollapse::ToRef)
}

/// Execute a WITH clause: rebind plain items into `computed`, or group+aggregate.
fn exec_with(
    wc: &WithClause,
    bindings: Vec<Binding>,
    graph: MergedGraph<'_>,
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

        // Aggregate positions for the WITH clause.
        let with_agg_specs: Vec<(String, bool, Option<&Expr>, AggregateKind, bool)> = agg_items
            .iter()
            .map(|ai| {
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
                    let is_cs = matches!(args.as_slice(), [Expr::Lit(Literal::Null)]);
                    let arg = if is_cs { None } else { args.first() };
                    let kind = AggregateKind::parse(name)
                        .expect("agg_items filtered to is_aggregate_fn names");
                    (col, is_cs, arg, kind, *distinct)
                } else {
                    unreachable!("agg_items filtered to FunCall aggregates")
                }
            })
            .collect();

        type WithGroupEntry = (Vec<(String, Value)>, Vec<Accumulator>);
        let mut groups: Vec<WithGroupEntry> = Vec::new();
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
            let slot = *key_index.entry(key_str).or_insert_with(|| {
                let accums = with_agg_specs
                    .iter()
                    .map(|(_, _, _, kind, distinct)| Accumulator::new(*kind, *distinct))
                    .collect();
                groups.push((key_pairs.clone(), accums));
                groups.len() - 1
            });
            let accums = &mut groups[slot].1;
            for (ai, (_, is_cs, arg_expr, _, _)) in with_agg_specs.iter().enumerate() {
                let v = if *is_cs {
                    Value::Null
                } else {
                    eval_expr(arg_expr.unwrap(), b, graph, cache)?
                };
                accums[ai].feed(v, *is_cs);
            }
        }

        // Produce one output Binding per group.
        let mut result = Vec::with_capacity(groups.len());
        for (key_pairs, accums) in groups {
            let mut computed: HashMap<String, Value> = HashMap::new();
            for (col, val) in key_pairs {
                computed.insert(col, val);
            }
            for ((col, _, _, _, _), accum) in with_agg_specs.iter().zip(accums) {
                computed.insert(col.clone(), accum.finalize());
            }
            result.push(Binding {
                node_vars: VarMap::default(),
                edge_vars: VarMap::default(),
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

/// Single source of truth for which FunCall names are aggregates. Parsed once
/// per FunCall (`AggregateKind::parse`) instead of re-checked via a hardcoded
/// string list at every classification site; `Accumulator::new` then matches
/// on the enum exhaustively, so adding a variant without wiring an accumulator
/// arm is a compile error instead of a silent fallback to `Counter(0)`.
/// Pre-uppercased — the parser normalizes (`parser.rs:382/398/572/588`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggregateKind {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Collect,
}

impl AggregateKind {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "COUNT" => Self::Count,
            "SUM" => Self::Sum,
            "AVG" => Self::Avg,
            "MIN" => Self::Min,
            "MAX" => Self::Max,
            "COLLECT" => Self::Collect,
            _ => return None,
        })
    }
}

/// Anything else parsed as a FunCall is treated as a scalar function
/// (`type(r)`, `id(n)`, `labels(n)`).
fn is_aggregate_fn(name: &str) -> bool {
    AggregateKind::parse(name).is_some()
}

/// Evaluate a scalar (non-aggregate) function call. Single dispatch point
/// shared by WHERE, plain RETURN, and rich-RETURN (WITH group-key) — a new
/// scalar function is wired here once and all three evaluators pick it up.
/// Returns `Value::Null` for unknown functions rather than erroring —
/// matches the OpenCypher convention that missing-data scalars degrade
/// gracefully (see graph_query rel-type `matches!` path). Supports the three
/// functions LLM agents reach for most: `type(r)` → edge rel-type as Str;
/// `id(n)` → node index as Int; `labels(n)` → single-element list of
/// node-kind Str.
fn eval_scalar_funcall(name: &str, args: &[Expr], b: &Binding, graph: MergedGraph<'_>) -> Value {
    let Some(Expr::Var(var)) = args.first() else {
        return Value::Null;
    };
    // `computed` takes precedence over node_vars/edge_vars whenever the var
    // is present there at all — same order `prop_value` uses. A WITH clause
    // that shadows a surviving name (`WITH b AS a`) only overwrites
    // `computed["a"]`; the plain-rebind branch of `exec_with` deliberately
    // preserves node_vars/edge_vars unchanged for downstream MATCH traversal,
    // so an old node_vars["a"] can still be sitting there stale. Falling
    // back to it after a `computed` miss would resolve the funcall against
    // the wrong entity — the pre-WITH one — while every other evaluator
    // (prop_value, eval_return_expr_with) already reads the new binding.
    if let Some(computed_val) = b.computed.get(var) {
        return match (name, computed_val) {
            ("TYPE", Value::EdgeRef { rel_type, .. }) => Value::Str(rel_type.as_str().into()),
            ("ID", Value::NodeRef { idx, .. }) => Value::Int(*idx as i64),
            ("LABELS", Value::NodeRef { kind, .. }) => Value::List(vec![Value::Str(kind.clone())]),
            _ => Value::Null,
        };
    }
    match name {
        "TYPE" => {
            let Some(&eidx) = b.edge_vars.get(var) else {
                return Value::Null;
            };
            if let Some(e) = graph.overlay_edge(eidx) {
                return Value::Str(e.rel_type.as_str().into());
            }
            let e = &graph.edges[eidx as usize];
            Value::Str(RelType::from(&e.rel_type).as_str().into())
        }
        "ID" => {
            let Some(&idx) = b.node_vars.get(var) else {
                return Value::Null;
            };
            Value::Int(idx as i64)
        }
        "LABELS" => {
            // labels(n) — single-kind list per ecp's one-label-per-node model.
            let Some(&idx) = b.node_vars.get(var) else {
                return Value::Null;
            };
            match graph.node(idx) {
                Some(m) => Value::List(vec![Value::Str(m.kind().as_str().into())]),
                None => Value::Null,
            }
        }
        _ => Value::Null,
    }
}

/// Per-aggregate in-place accumulator — eliminates per-row `Binding` clones from
/// the grouping loop. Each variant holds only the running state needed to produce
/// the final scalar; no `Binding` references are retained after the row is consumed.
///
/// `u64` for Counter: the JVM Guava corpus has 55k+ Method nodes and a `usize`
/// counter would require an explicit cast on finalize; `u64` makes the intent clear.
enum Accumulator {
    Counter(u64),
    CounterDistinct(HashSet<String>),
    Summer {
        sum_i: i64,
        sum_f: f64,
        has_float: bool,
    },
    MinAccum(Option<Value>),
    MaxAccum(Option<Value>),
    Collector(Vec<Value>),
    CollectorDistinct(Vec<Value>, HashSet<String>),
    Avg {
        sum: f64,
        count: u64,
    },
}

impl Accumulator {
    /// Exhaustive over `AggregateKind` — a new variant without a matching arm
    /// here is a compile error, not a silent fallback to `Counter(0)`.
    fn new(kind: AggregateKind, distinct: bool) -> Self {
        match kind {
            AggregateKind::Count => {
                if distinct {
                    Accumulator::CounterDistinct(HashSet::new())
                } else {
                    Accumulator::Counter(0)
                }
            }
            AggregateKind::Sum => Accumulator::Summer {
                sum_i: 0,
                sum_f: 0.0,
                has_float: false,
            },
            AggregateKind::Min => Accumulator::MinAccum(None),
            AggregateKind::Max => Accumulator::MaxAccum(None),
            AggregateKind::Collect => {
                if distinct {
                    Accumulator::CollectorDistinct(Vec::new(), HashSet::new())
                } else {
                    Accumulator::Collector(Vec::new())
                }
            }
            AggregateKind::Avg => Accumulator::Avg { sum: 0.0, count: 0 },
        }
    }

    /// Feed one row's projected value into this accumulator.
    /// `is_count_star`: if true this is `COUNT(*)` — value is ignored, row itself counts.
    fn feed(&mut self, v: Value, is_count_star: bool) {
        match self {
            Accumulator::Counter(n) => {
                if is_count_star || !matches!(v, Value::Null) {
                    *n += 1;
                }
            }
            Accumulator::CounterDistinct(seen) => {
                if !matches!(v, Value::Null) {
                    seen.insert(value_key(&v));
                }
            }
            Accumulator::Summer {
                sum_i,
                sum_f,
                has_float,
            } => match v {
                Value::Int(i) => *sum_i += i,
                Value::Float(f) => {
                    *sum_f += f;
                    *has_float = true;
                }
                _ => {}
            },
            Accumulator::MinAccum(cur) => {
                if !matches!(v, Value::Null) {
                    *cur = Some(match cur.take() {
                        None => v,
                        Some(prev) => {
                            if eval_binop(Op::Lt, &v, &prev) {
                                v
                            } else {
                                prev
                            }
                        }
                    });
                }
            }
            Accumulator::MaxAccum(cur) => {
                if !matches!(v, Value::Null) {
                    *cur = Some(match cur.take() {
                        None => v,
                        Some(prev) => {
                            if eval_binop(Op::Gt, &v, &prev) {
                                v
                            } else {
                                prev
                            }
                        }
                    });
                }
            }
            Accumulator::Collector(items) => {
                if !matches!(v, Value::Null) {
                    items.push(v);
                }
            }
            Accumulator::CollectorDistinct(items, seen) => {
                if !matches!(v, Value::Null) {
                    let k = value_key(&v);
                    if seen.insert(k) {
                        items.push(v);
                    }
                }
            }
            Accumulator::Avg { sum, count } => match v {
                Value::Int(i) => {
                    *sum += i as f64;
                    *count += 1;
                }
                Value::Float(f) => {
                    *sum += f;
                    *count += 1;
                }
                _ => {}
            },
        }
    }

    fn finalize(self) -> Value {
        match self {
            Accumulator::Counter(n) => Value::Int(n as i64),
            Accumulator::CounterDistinct(seen) => Value::Int(seen.len() as i64),
            Accumulator::Summer {
                sum_i,
                sum_f,
                has_float,
            } => {
                if has_float {
                    Value::Float(sum_f + sum_i as f64)
                } else {
                    Value::Int(sum_i)
                }
            }
            Accumulator::MinAccum(v) => v.unwrap_or(Value::Null),
            Accumulator::MaxAccum(v) => v.unwrap_or(Value::Null),
            Accumulator::Collector(items) => Value::List(items),
            Accumulator::CollectorDistinct(items, _) => Value::List(items),
            Accumulator::Avg { sum, count } => {
                if count == 0 {
                    Value::Null
                } else {
                    Value::Float(sum / count as f64)
                }
            }
        }
    }
}

fn exec_match_clause(
    mc: &MatchClause,
    prior: &[Binding],
    graph: MergedGraph<'_>,
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
/// Push every overlay virtual node matching `np` — the virtual complement of
/// the base scans (which skip suppressed/replaced base indices). Linear over
/// O(dirty symbols); no-op without a view.
fn seed_virtuals(graph: MergedGraph<'_>, np: &NodePat, mut push: impl FnMut(u32)) {
    if let Some(v) = graph.view() {
        for i in 0..v.virtual_nodes().len() as u32 {
            let idx = graph.base_len() + i;
            if node_matches(idx, np, graph) {
                push(idx);
            }
        }
    }
}

fn exec_pattern(
    pat: &Pattern,
    base: &Binding,
    graph: MergedGraph<'_>,
) -> Result<Vec<Binding>, CypherError> {
    // If the first node is unbound but the last one is already bound, walk the
    // pattern right-to-left: reverse nodes/rels and invert each hop direction.
    // Seeding from the bound endpoint replaces a full-node scan per prior row
    // with one adjacency walk — `OPTIONAL MATCH (c)-[:Calls]->(f)` with `f`
    // bound (the orphan-query shape) is O(deg f) instead of O(V+E).
    let bound_in_base = |np: &NodePat| {
        np.var
            .as_deref()
            .is_some_and(|v| base.node_vars.get(v).is_some())
    };
    if pat.nodes.len() > 1
        && !bound_in_base(&pat.nodes[0])
        && bound_in_base(&pat.nodes[pat.nodes.len() - 1])
    {
        return exec_pattern(&invert_pattern(pat), base, graph);
    }

    // Frontier: (binding, last_matched_node_idx)
    let mut frontier: Vec<(Binding, u32)> = Vec::new();
    let first_np = &pat.nodes[0];

    // Hotspot 1: when the first node carries a kind filter, iterate via the
    // v10 `kind_offsets` CSR slice directly — `MATCH (m:Method)` against a
    // 303k-node graph then visits only the ~110k Method indices. The CSR
    // path uses a concrete slice iterator (no `Box<dyn Iterator>` vcall in
    // the inner loop). Empty-kinds and legacy-v9 (no CSR) fall through to
    // the full linear scan that mirrors the previous behaviour.
    let csr_ready = !graph.kind_offsets.is_empty();
    let use_kind_csr = csr_ready
        && !first_np.kinds.is_empty()
        && first_np.kinds.iter().all(|k| {
            let kidx = k.as_index();
            graph.kind_offsets.len() > kidx + 1
        });

    // If the first node var is already bound, pin to that node only.
    if let Some(var) = &first_np.var {
        if let Some(&already) = base.node_vars.get(var) {
            if node_matches(already, first_np, graph) {
                frontier.push((base.clone(), already));
            }
        } else if use_kind_csr {
            for &kind in &first_np.kinds {
                let kidx = kind.as_index();
                let start = graph.kind_offsets[kidx].to_native() as usize;
                let end = graph.kind_offsets[kidx + 1].to_native() as usize;
                for &raw in &graph.kind_node_idx[start..end] {
                    let idx = raw.to_native();
                    if !graph.base_visible(idx) || !node_matches(idx, first_np, graph) {
                        continue;
                    }
                    let mut b = base.clone();
                    b.node_vars.insert(var, idx);
                    frontier.push((b, idx));
                }
            }
            seed_virtuals(graph, first_np, |idx| {
                let mut b = base.clone();
                b.node_vars.insert(var, idx);
                frontier.push((b, idx));
            });
        } else {
            for idx in 0..graph.nodes.len() as u32 {
                if !graph.base_visible(idx) || !node_matches(idx, first_np, graph) {
                    continue;
                }
                let mut b = base.clone();
                b.node_vars.insert(var, idx);
                frontier.push((b, idx));
            }
            seed_virtuals(graph, first_np, |idx| {
                let mut b = base.clone();
                b.node_vars.insert(var, idx);
                frontier.push((b, idx));
            });
        }
    } else if use_kind_csr {
        for &kind in &first_np.kinds {
            let kidx = kind.as_index();
            let start = graph.kind_offsets[kidx].to_native() as usize;
            let end = graph.kind_offsets[kidx + 1].to_native() as usize;
            for &raw in &graph.kind_node_idx[start..end] {
                let idx = raw.to_native();
                if !graph.base_visible(idx) || !node_matches(idx, first_np, graph) {
                    continue;
                }
                frontier.push((base.clone(), idx));
            }
        }
        seed_virtuals(graph, first_np, |idx| frontier.push((base.clone(), idx)));
    } else {
        // Anonymous first node: scan all nodes.
        for idx in 0..graph.nodes.len() as u32 {
            if !graph.base_visible(idx) || !node_matches(idx, first_np, graph) {
                continue;
            }
            frontier.push((base.clone(), idx));
        }
        seed_virtuals(graph, first_np, |idx| frontier.push((base.clone(), idx)));
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
                        if !node_matches(tgt_idx, next_np, graph) {
                            continue;
                        }
                        // A var already bound earlier pins the hop target —
                        // filter on it, never rebind.
                        if let Some(var) = &next_np.var {
                            if b.node_vars
                                .get(var)
                                .is_some_and(|&pinned| pinned != tgt_idx)
                            {
                                continue;
                            }
                        }
                        let mut nb = b.clone();
                        if let Some(var) = &next_np.var {
                            nb.node_vars.insert(var, tgt_idx);
                        }
                        if let Some(var) = &rel.var {
                            if let Some(ei) = edge_idx_opt {
                                nb.edge_vars.insert(var, ei);
                            }
                        }
                        next_frontier.push((nb, tgt_idx));
                    }
                }
                // Single-hop
                None => {
                    walk_rel(*cur_idx, rel, graph, |tgt_idx, edge_idx| {
                        if !node_matches(tgt_idx, next_np, graph) {
                            return;
                        }
                        // A var already bound earlier pins the hop target —
                        // filter on it, never rebind.
                        if let Some(var) = &next_np.var {
                            if b.node_vars
                                .get(var)
                                .is_some_and(|&pinned| pinned != tgt_idx)
                            {
                                return;
                            }
                        }
                        let mut nb = b.clone();
                        if let Some(var) = &next_np.var {
                            nb.node_vars.insert(var, tgt_idx);
                        }
                        if let Some(var) = &rel.var {
                            nb.edge_vars.insert(var, edge_idx);
                        }
                        next_frontier.push((nb, tgt_idx));
                    });
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
    graph: MergedGraph<'_>,
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
        walk_rel(idx, rel, graph, |tgt, edge_idx| {
            if visited.insert(tgt) {
                queue.push_back((tgt, depth + 1, Some(edge_idx)));
            }
        });
    }
    out
}

/// Merged-space node filter: `node_idx` may be a base or virtual index.
fn node_matches(node_idx: u32, np: &NodePat, graph: MergedGraph<'_>) -> bool {
    let Some(m) = graph.node(node_idx) else {
        return false;
    };
    // Zero-cost discriminant read; reused by both label filter and prop_value below.
    let kind = m.kind();
    if !np.kinds.is_empty() && !np.kinds.contains(&kind) {
        return false;
    }
    for (key, lit) in &np.props {
        // Hot-path allocation-free checks for the three most common inline-map keys.
        // All other properties fall through to the general path via node_prop_no_cache,
        // which mirrors the full WHERE property set (minus "content" which requires a
        // file read and is intentionally excluded from inline-map filtering).
        match key.as_str() {
            "name" => {
                let n = m.name(&graph);
                if !matches!(lit, Literal::Str(s) if n == s.as_str()) {
                    return false;
                }
            }
            "kind" => {
                if !matches!(lit, Literal::Str(s) if kind.as_str() == s.as_str()) {
                    return false;
                }
            }
            "uid" => {
                if !matches!(lit, Literal::Int(v) if m.uid() as i64 == *v) {
                    return false;
                }
            }
            other => {
                let val = node_prop_no_cache(node_idx, other, graph);
                if !literal_matches_value(lit, &val) {
                    return false;
                }
            }
        }
    }
    true
}

/// Evaluate a node property without requiring a `ContentCache`.
/// Mirrors `node_prop_value` for all properties except `"content"` (which
/// requires a file read) — returns `Value::Null` for unknown or excluded keys
/// so that `literal_matches_value` falls through to `false` (no match).
/// "name", "kind", "uid" are handled by the hot-3 in `node_matches` and
/// never reach this function.
fn node_prop_no_cache(node_idx: u32, prop: &str, graph: MergedGraph<'_>) -> Value {
    let Some(m) = graph.node(node_idx) else {
        return Value::Null;
    };
    // Function-meta side tables are span/idx-keyed on the BASE graph: a
    // replaced virtual node reads its uid-identical base twin; a brand-new
    // symbol has no metas yet and takes the sparse defaults.
    let fm = m.meta_idx(node_idx);
    match prop {
        "filePath" => Value::Str(m.file_path(&graph).unwrap_or("").into()),
        "ownerClass" => match m.owner_class(&graph) {
            Some(oc) => Value::Str(oc.into()),
            None => Value::Null,
        },
        "line" | "startLine" => Value::Int(m.start_line() as i64),
        "endLine" => Value::Int(m.end_line() as i64),
        "is_test" | "isTest" => {
            Value::Bool(fm_flag(graph, fm, crate::graph::FunctionMeta::FLAG_TEST))
        }
        "is_async" | "isAsync" => {
            Value::Bool(fm_flag(graph, fm, crate::graph::FunctionMeta::FLAG_ASYNC))
        }
        "is_static" | "isStatic" => {
            Value::Bool(fm_flag(graph, fm, crate::graph::FunctionMeta::FLAG_STATIC))
        }
        "is_abstract" | "isAbstract" => Value::Bool(fm_flag(
            graph,
            fm,
            crate::graph::FunctionMeta::FLAG_ABSTRACT,
        )),
        "is_generator" | "isGenerator" => Value::Bool(fm_flag(
            graph,
            fm,
            crate::graph::FunctionMeta::FLAG_GENERATOR,
        )),
        "is_extern" | "isExtern" => {
            Value::Bool(fm_flag(graph, fm, crate::graph::FunctionMeta::FLAG_EXTERN))
        }
        "visibility" => Value::Int(fm.map_or(0, |i| archived_fm_visibility(graph, i)) as i64),
        // "content" excluded: requires file I/O; use WHERE n.content = … instead.
        // "decorators" is a list — inline-map equality against a list literal is
        // an edge case handled by literal_matches_value's List arm.
        "decorators" => fm
            .map(|i| archived_fm_decorators(graph, i))
            .unwrap_or(Value::List(Vec::new())),
        // Unknown property or "content": return Null so literal_matches_value → false.
        _ => Value::Null,
    }
}

/// True when a `Literal` filter value matches a property `Value`.
/// `Value::Null` (missing/unsupported property) never matches any literal.
fn literal_matches_value(lit: &Literal, val: &Value) -> bool {
    match (lit, val) {
        (Literal::Str(ls), Value::Str(vs)) => ls == vs,
        (Literal::Int(li), Value::Int(vi)) => *li == *vi,
        (Literal::Float(lf), Value::Float(vf)) => *lf == *vf,
        (Literal::Bool(lb), Value::Bool(vb)) => *lb == *vb,
        (Literal::Null, Value::Null) => true,
        _ => false,
    }
}

/// Walk one hop from `from` in the given direction, invoking `emit(target_idx, edge_idx)`
/// per matching edge. Closure-based instead of returning `Vec` so the frontier-expansion
/// loop in `exec_pattern` doesn't pay a per-source-node allocation — at 110k+ source nodes
/// the cumulative `Vec::new()` cost was ~6 ms of edge-traversal query time.
/// Merged-space hop: `from` may be a base or virtual index, and the emitted
/// targets and edge indices are merged-space too. The overlay merge itself
/// belongs to [`MergedGraph::out_edges`] / [`MergedGraph::in_edges`]; what is
/// left here is the rel-type filter.
fn walk_rel<F: FnMut(u32, u32)>(from: u32, rel: &RelPat, graph: MergedGraph<'_>, mut emit: F) {
    let type_ok = |rt: RelType| -> bool { rel.types.is_empty() || rel.types.contains(&rt) };

    if matches!(rel.dir, Direction::Out | Direction::Both) {
        for edge in graph.out_edges(from) {
            if type_ok(edge.rel_type()) {
                emit(edge.target, edge.idx);
            }
        }
    }
    if matches!(rel.dir, Direction::In | Direction::Both) {
        for edge in graph.in_edges(from) {
            if type_ok(edge.rel_type()) {
                emit(edge.source, edge.idx);
            }
        }
    }
}

fn eval_expr(
    e: &Expr,
    b: &Binding,
    graph: MergedGraph<'_>,
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
                return Ok(match graph.node(idx) {
                    Some(m) => Value::Str(m.name(&graph).into()),
                    None => Value::Null,
                });
            }
            // Edge variables must resolve to non-Null so aggregates like
            // `count(r)` and `count(DISTINCT r)` see a value per binding.
            // Returns EdgeRef (same shape as the rich projection path uses)
            // so `value_key` partitions on edge identity for DISTINCT.
            if let Some(&eidx) = b.edge_vars.get(var) {
                if let Some(e) = graph.overlay_edge(eidx) {
                    return Ok(Value::EdgeRef {
                        src: e.source,
                        tgt: e.target,
                        rel_type: e.rel_type,
                        confidence: e.confidence,
                        reason: crate::session::OVERLAY_EDGE_REASON.to_string(),
                    });
                }
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
            // Pushdown: `<StringLiteral> IN <NodeVar>.decorators` is the canonical
            // decorator-filter shape fired by agents on every query. Recognizing it
            // here avoids materializing the entire Value::List (binary_search +
            // N string allocs) and instead walks the archived rkyv slice directly.
            // The generic path remains correct for every other InCollection shape.
            if let Some((var, needle)) = const_str_in_decorators(scalar, collection) {
                let node_idx = b.node_vars.get(var).copied().or_else(|| {
                    // WITH-rebind may store a NodeRef in computed.
                    b.computed.get(var).and_then(|v| {
                        if let Value::NodeRef { idx, .. } = v {
                            Some(*idx)
                        } else {
                            None
                        }
                    })
                });
                if let Some(idx) = node_idx {
                    return Ok(Value::Bool(archived_decorator_contains(graph, idx, needle)));
                }
                // var is unbound or not a node — generic path produces correct Null/false.
            }
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
            // Category labels were normalized to member kind names at parse
            // time, so this stays an allocation-free per-row string compare.
            let Some(m) = graph.node(idx) else {
                return Ok(Value::Null);
            };
            let kind_str = m.kind().as_str();
            Ok(Value::Bool(labels.iter().any(|l| l == kind_str)))
        }
        IsNull { expr, negated } => {
            let v = eval_expr(expr, b, graph, cache)?;
            let is_null = matches!(v, Value::Null);
            Ok(Value::Bool(is_null ^ negated))
        }
        ExistsPattern { pattern, negated } => {
            Ok(Value::Bool(pattern_exists(pattern, b, graph)? ^ negated))
        }
        FunCall { name, args, .. } => {
            // Aggregates have no meaning against a single row's binding — same
            // restriction as OpenCypher (`WHERE count(n) > 1` is a semantic
            // error there too, not a WHERE-specific gap in this executor).
            if is_aggregate_fn(name) {
                return Err(CypherError::Exec {
                    msg: format!("aggregate function {name}() not allowed in WHERE"),
                });
            }
            Ok(eval_scalar_funcall(name, args, b, graph))
        }
    }
}

/// Returns `true` if `pattern` matches at least one path in `graph` given the
/// current variable bindings in `b`.
///
/// Shapes, fastest first:
/// - Bare node pattern (`rels` empty): true iff the node var is bound.
/// - Single-hop fixed-length `(a)-[r]->(b)` with a bound endpoint: anchors on
///   it, walks that adjacency list, short-circuits on the first match.
/// - Single-hop fixed-length with NO bound endpoint ("does any such edge
///   exist"): linear edge scan, short-circuiting on the first match.
/// - Variable-length (`*min..max`) or multi-hop: delegated to the full
///   pattern walker — `exec_pattern` anchors on a bound endpoint via its
///   right-to-left reversal, but materialises matches rather than
///   short-circuiting, so it requires at least one bound variable.
fn pattern_exists(
    pattern: &Pattern,
    b: &Binding,
    graph: MergedGraph<'_>,
) -> Result<bool, CypherError> {
    if pattern.rels.is_empty() {
        return Ok(pattern
            .nodes
            .first()
            .and_then(|n| n.var.as_deref())
            .is_some_and(|v| b.node_vars.contains_key(v)));
    }
    if pattern.rels.len() == 1 && pattern.rels[0].range.is_none() {
        let n0 = &pattern.nodes[0];
        let n1 = &pattern.nodes[1];
        let rel = &pattern.rels[0];
        let bound0 = n0.var.as_deref().and_then(|v| b.node_vars.get(v)).copied();
        let bound1 = n1.var.as_deref().and_then(|v| b.node_vars.get(v)).copied();
        let (anchor, target_pat, dir) = match (bound0, bound1) {
            (Some(idx), _) => (idx, n1, rel.dir),
            (None, Some(idx)) => (idx, n0, invert_dir(rel.dir)),
            (None, None) => {
                return Ok(edge_scan_exists(n0, n1, rel, graph));
            }
        };
        let probe = RelPat {
            var: rel.var.clone(),
            types: rel.types.clone(),
            range: rel.range,
            dir,
        };
        let mut found = false;
        walk_rel(anchor, &probe, graph, |tgt, _e| {
            if found {
                return;
            }
            if node_matches(tgt, target_pat, graph) {
                found = true;
            }
        });
        return Ok(found);
    }

    let any_bound = pattern.nodes.iter().any(|n| {
        n.var
            .as_deref()
            .is_some_and(|v| b.node_vars.get(v).is_some())
    });
    if !any_bound {
        return Err(CypherError::Exec {
            msg: "EXISTS over a multi-hop or variable-length pattern needs at least one \
                  variable bound by an outer MATCH"
                .into(),
        });
    }
    exists_dfs(pattern, b, graph)
}

/// "Does any edge match" for a single-hop pattern with no bound endpoint:
/// scan the flat edge slice and short-circuit on the first hit.
fn edge_scan_exists(n0: &NodePat, n1: &NodePat, rel: &RelPat, graph: MergedGraph<'_>) -> bool {
    let fits = |a: u32, pa: &NodePat, z: u32, pz: &NodePat| {
        node_matches(a, pa, graph) && node_matches(z, pz, graph)
    };
    graph.all_edges().any(|edge| {
        if !rel.types.is_empty() && !rel.types.contains(&edge.rel_type()) {
            return false;
        }
        let (s, t) = (edge.source, edge.target);
        match rel.dir {
            Direction::Out => fits(s, n0, t, n1),
            Direction::In => fits(t, n0, s, n1),
            Direction::Both => fits(s, n0, t, n1) || fits(t, n0, s, n1),
        }
    })
}

fn invert_dir(d: Direction) -> Direction {
    match d {
        Direction::Out => Direction::In,
        Direction::In => Direction::Out,
        Direction::Both => Direction::Both,
    }
}

/// The same pattern walked right-to-left: nodes reversed, hops reversed with
/// each direction inverted. Matching semantics are identical; only the seed
/// endpoint changes.
fn invert_pattern(pat: &Pattern) -> Pattern {
    Pattern {
        nodes: pat.nodes.iter().rev().cloned().collect(),
        rels: pat
            .rels
            .iter()
            .rev()
            .map(|r| RelPat {
                var: r.var.clone(),
                types: r.types.clone(),
                range: r.range,
                dir: invert_dir(r.dir),
            })
            .collect(),
    }
}

/// Find-first DFS behind EXISTS for multi-hop / variable-length patterns:
/// answers "does at least one full match exist" and stops at the first one,
/// instead of materialising every per-hop binding like `exec_pattern`.
/// On a high-fan-in pattern the difference is one path versus the full
/// k-hop expansion.
///
/// Vars bound in `base` pin their hop targets; vars introduced inside the
/// pattern are tracked in `local` so a var repeated within the pattern
/// (a cycle probe like `(x)-->(y)-->(x)`) stays consistent. Edge vars are
/// ignored — a boolean answer never reads them.
fn exists_dfs(pat: &Pattern, base: &Binding, graph: MergedGraph<'_>) -> Result<bool, CypherError> {
    let bound_in_base = |np: &NodePat| {
        np.var
            .as_deref()
            .is_some_and(|v| base.node_vars.get(v).is_some())
    };
    if pat.nodes.len() > 1
        && !bound_in_base(&pat.nodes[0])
        && bound_in_base(&pat.nodes[pat.nodes.len() - 1])
    {
        return exists_dfs(&invert_pattern(pat), base, graph);
    }

    fn hop(
        pat: &Pattern,
        depth: usize,
        cur: u32,
        base: &Binding,
        local: &mut Vec<(String, u32)>,
        graph: MergedGraph<'_>,
    ) -> bool {
        if depth == pat.rels.len() {
            return true;
        }
        let rel = &pat.rels[depth];
        let next_np = &pat.nodes[depth + 1];
        let pinned = next_np.var.as_deref().and_then(|v| {
            base.node_vars
                .get(v)
                .copied()
                .or_else(|| local.iter().find(|(k, _)| k == v).map(|(_, i)| *i))
        });
        // Collect this hop's neighbours (degree-bounded) so the recursion
        // doesn't run inside walk_rel's borrow.
        let mut targets: Vec<u32> = Vec::new();
        match rel.range {
            Some((min, max)) => {
                for (tgt, _e) in bfs_var_len(cur, rel, graph, min, max) {
                    targets.push(tgt);
                }
            }
            None => walk_rel(cur, rel, graph, |tgt, _e| targets.push(tgt)),
        }
        for tgt in targets {
            if pinned.is_some_and(|p| p != tgt) {
                continue;
            }
            if !node_matches(tgt, next_np, graph) {
                continue;
            }
            let introduced = match next_np.var.as_deref() {
                Some(v) if pinned.is_none() => {
                    local.push((v.to_string(), tgt));
                    true
                }
                _ => false,
            };
            if hop(pat, depth + 1, tgt, base, local, graph) {
                return true;
            }
            if introduced {
                local.pop();
            }
        }
        false
    }

    let first_np = &pat.nodes[0];
    let mut local: Vec<(String, u32)> = Vec::new();
    let try_seed = |idx: u32, local: &mut Vec<(String, u32)>| -> bool {
        if !node_matches(idx, first_np, graph) {
            return false;
        }
        let introduced = match first_np.var.as_deref() {
            Some(v) if base.node_vars.get(v).is_none() => {
                local.push((v.to_string(), idx));
                true
            }
            _ => false,
        };
        let hit = hop(pat, 0, idx, base, local, graph);
        if introduced {
            local.pop();
        }
        hit
    };

    if let Some(var) = first_np.var.as_deref() {
        if let Some(&idx) = base.node_vars.get(var) {
            return Ok(try_seed(idx, &mut local));
        }
    }
    // Mirror exec_pattern's seeding: kind-CSR slice when available, else a
    // full scan — but short-circuit on the first seed that completes.
    let use_kind_csr = !graph.kind_offsets.is_empty()
        && !first_np.kinds.is_empty()
        && first_np
            .kinds
            .iter()
            .all(|k| graph.kind_offsets.len() > k.as_index() + 1);
    let virtual_range = || graph.base_len()..graph.node_count();
    if use_kind_csr {
        for &kind in &first_np.kinds {
            let kidx = kind.as_index();
            let start = graph.kind_offsets[kidx].to_native() as usize;
            let end = graph.kind_offsets[kidx + 1].to_native() as usize;
            for &raw in &graph.kind_node_idx[start..end] {
                let idx = raw.to_native();
                if graph.base_visible(idx) && try_seed(idx, &mut local) {
                    return Ok(true);
                }
            }
        }
        for idx in virtual_range() {
            if try_seed(idx, &mut local) {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    for idx in 0..graph.nodes.len() as u32 {
        if graph.base_visible(idx) && try_seed(idx, &mut local) {
            return Ok(true);
        }
    }
    for idx in virtual_range() {
        if try_seed(idx, &mut local) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn lit_to_value(l: &Literal) -> Value {
    match l {
        Literal::Null => Value::Null,
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Int(i) => Value::Int(*i),
        Literal::Float(f) => Value::Float(*f),
        Literal::Str(s) => Value::Str(s.as_str().into()),
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
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Value {
    // Check computed values first (set by WITH clause).
    if let Some(computed_val) = b.computed.get(var) {
        return match computed_val {
            // If the computed value is a NodeRef, resolve the property from the graph.
            Value::NodeRef { idx, .. } => node_prop_value(*idx, prop, graph, cache),
            // EdgeRef: resolve edge properties.
            Value::EdgeRef {
                src: _,
                tgt: _,
                rel_type,
                confidence,
                reason,
            } => match prop {
                "confidence" => Value::Float(*confidence as f64),
                "reason" => Value::Str(reason.as_str().into()),
                "rel_type" => Value::Str(format!("{rel_type:?}").into()),
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
        return node_prop_value(idx, prop, graph, cache);
    }
    if let Some(&edge_idx) = b.edge_vars.get(var) {
        if let Some(e) = graph.overlay_edge(edge_idx) {
            return match prop {
                "confidence" => Value::Float(e.confidence as f64),
                "reason" => Value::Str(crate::session::OVERLAY_EDGE_REASON.into()),
                "rel_type" => Value::Str(e.rel_type.as_str().into()),
                _ => Value::Null,
            };
        }
        let e = &graph.edges[edge_idx as usize];
        return match prop {
            "confidence" => Value::Float(e.confidence.to_native() as f64),
            "reason" => Value::Str(e.reason.resolve(&graph.string_pool).into()),
            "rel_type" => Value::Str(RelType::from(&e.rel_type).as_str().into()),
            _ => Value::Null,
        };
    }
    Value::Null
}

/// Resolve a single property from an archived node.
/// `node_idx` is the position of `n` in `graph.nodes` — needed for the sparse
/// `function_metas` binary-search lookup.
/// `cache` is used for the `content` property (C12).
///
/// The set of names matched here is mirrored by `diagnostics::KNOWN_NODE_PROPS`
/// (the unknown-property warning). A new arm added below must be added there
/// too, or legal queries using it will false-positive as unknown.
fn node_prop_value(
    node_idx: u32,
    prop: &str,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Value {
    let Some(m) = graph.node(node_idx) else {
        return Value::Null;
    };
    let fm = m.meta_idx(node_idx);
    match prop {
        "name" => Value::Str(m.name(&graph).into()),
        // u64 uid stored as i64 bits — no allocation per row.
        "uid" => Value::Int(m.uid() as i64),
        "ownerClass" => match m.owner_class(&graph) {
            Some(oc) => Value::Str(oc.into()),
            None => Value::Null,
        },
        "kind" => Value::Str(m.kind().as_str().into()),
        // 1-based, matching impact/find/inspect output (see Node::start_line).
        // `span.0` is the raw 0-based tree-sitter row; never expose it as `line`.
        "line" | "startLine" => Value::Int(m.start_line() as i64),
        "endLine" => Value::Int(m.end_line() as i64),
        "filePath" => Value::Str(m.file_path(&graph).unwrap_or("").into()),
        "content" => {
            let slice = match &m {
                MergedNode::Base(n) => {
                    // Lazy file read + span slice.
                    let file_idx = n.file_idx.to_native();
                    let span = (
                        n.span.0.to_native(),
                        n.span.1.to_native(),
                        n.span.2.to_native(),
                        n.span.3.to_native(),
                    );
                    cache
                        .body_for_file(&graph, file_idx)
                        .map(|body| slice_by_span(body, span))
                        .unwrap_or_default()
                }
                // Virtual node: the dirty file on disk is the truth, and the
                // fragment carries line-resolution spans only — slice whole
                // lines. Uncached read, bounded by dirty symbols per query.
                MergedNode::Virtual(vn) => {
                    std::fs::read_to_string(cache.repo_root.join(vn.rel_path.as_ref()))
                        .map(|body| {
                            let start = vn.start_line.saturating_sub(1) as usize;
                            let take =
                                (vn.end_line.max(vn.start_line) - vn.start_line + 1) as usize;
                            body.lines()
                                .skip(start)
                                .take(take)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default()
                }
            };
            Value::Str(slice.into())
        }
        // ── FunctionMeta flag properties ────────────────────────────────────
        // FunctionMeta is sparse (only Function/Method/Constructor nodes).
        // Nodes without a record return safe defaults (false / 0 / empty list)
        // so WHERE m.is_async = true works without needing a Null check.
        "is_test" | "isTest" => {
            Value::Bool(fm_flag(graph, fm, crate::graph::FunctionMeta::FLAG_TEST))
        }
        "is_async" | "isAsync" => {
            Value::Bool(fm_flag(graph, fm, crate::graph::FunctionMeta::FLAG_ASYNC))
        }
        "is_static" | "isStatic" => {
            Value::Bool(fm_flag(graph, fm, crate::graph::FunctionMeta::FLAG_STATIC))
        }
        "is_abstract" | "isAbstract" => Value::Bool(fm_flag(
            graph,
            fm,
            crate::graph::FunctionMeta::FLAG_ABSTRACT,
        )),
        "is_generator" | "isGenerator" => Value::Bool(fm_flag(
            graph,
            fm,
            crate::graph::FunctionMeta::FLAG_GENERATOR,
        )),
        "is_extern" | "isExtern" => {
            Value::Bool(fm_flag(graph, fm, crate::graph::FunctionMeta::FLAG_EXTERN))
        }
        "visibility" => Value::Int(fm.map_or(0, |i| archived_fm_visibility(graph, i)) as i64),
        "decorators" => fm
            .map(|i| archived_fm_decorators(graph, i))
            .unwrap_or(Value::List(Vec::new())),
        _ => Value::Null,
    }
}

/// `archived_fm_flag` over an optional BASE index (`MergedNode::meta_idx`): a
/// brand-new virtual symbol has no meta record and takes the sparse default.
fn fm_flag(graph: MergedGraph<'_>, base_idx: Option<u32>, flag: u16) -> bool {
    base_idx.is_some_and(|i| archived_fm_flag(graph, i, flag))
}

/// Return true when the node's FunctionMeta has the given flag set.
/// Nodes with no FunctionMeta record return false (sparse-record default).
fn archived_fm_flag(graph: MergedGraph<'_>, node_idx: u32, flag: u16) -> bool {
    if flag <= u8::MAX as u16 && graph.node_flags.len() > node_idx as usize {
        return graph.node_flags[node_idx as usize] & flag as u8 != 0;
    }

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
fn archived_fm_visibility(graph: MergedGraph<'_>, node_idx: u32) -> u8 {
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
fn archived_fm_decorators(graph: MergedGraph<'_>, node_idx: u32) -> Value {
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
                Value::Str(normalized.into())
            })
            .collect(),
        Err(_) => vec![],
    };
    Value::List(items)
}

/// If `scalar IN collection` matches the shape `Lit(Str(needle)) IN Prop(var, "decorators")`,
/// return `(var_name, needle_str)`. The restriction to exactly `"decorators"` keeps the
/// pushdown narrow — only the proven hot pattern; all other shapes fall through.
fn const_str_in_decorators<'e>(
    scalar: &'e Expr,
    collection: &'e Expr,
) -> Option<(&'e str, &'e str)> {
    let needle = match scalar {
        Expr::Lit(Literal::Str(s)) => s.as_str(),
        _ => return None,
    };
    match collection {
        Expr::Prop(var, prop) if prop == "decorators" => Some((var.as_str(), needle)),
        _ => None,
    }
}

/// Walk the archived decorator slice for `node_idx` and return true if any entry,
/// after stripping a leading `@`, equals `needle`. Zero heap allocation — reads
/// directly from the rkyv-archived string pool.
fn archived_decorator_contains(graph: MergedGraph<'_>, node_idx: u32, needle: &str) -> bool {
    match graph
        .function_metas
        .binary_search_by_key(&node_idx, |m| m.node_idx.to_native())
    {
        Ok(i) => graph.function_metas[i].decorators.iter().any(|d| {
            let s = d.resolve(&graph.string_pool);
            s.strip_prefix('@').unwrap_or(s) == needle
        }),
        Err(_) => false,
    }
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
    // `NodeKind` is no longer imported by the parent module — the merged-node
    // accessors that used it moved to `session::merged`.
    use crate::graph::{NodeKind, ZeroCopyGraph};
    use crate::graph_fixture::GraphFixture;

    // -----------------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------------

    /// Two-node fixture: `caller`(0) -[:Calls]-> `callee`(1), one file.
    fn two_node_graph() -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        let caller = fx.func("src/x.ts", "caller");
        fx.span(caller, (0, 0, 5, 1));
        let callee = fx.func("src/x.ts", "callee");
        fx.span(callee, (6, 0, 8, 1));
        fx.edge(caller, callee, RelType::Calls);
        fx.build()
    }

    /// The same graph with the v10 kind CSR stripped — the shape a v9 cache
    /// upgraded in place has, and the one that exercises the linear-scan
    /// fallback in `nodes_by_kind`. `build_two_node_with_csr` is the pair.
    fn build_two_node() -> Vec<u8> {
        let mut g = two_node_graph();
        g.kind_offsets.clear();
        g.kind_node_idx.clear();
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
        let mut fx = GraphFixture::new();
        let a = fx.func("src/x.ts", "a");
        fx.span(a, (0, 0, 1, 0));
        let b = fx.func("src/x.ts", "b");
        fx.span(b, (2, 0, 3, 0));
        let c = fx.func("src/x.ts", "c");
        fx.span(c, (4, 0, 5, 0));
        fx.edge_with(a, b, RelType::Calls, 1.0, "r1");
        fx.edge_with(b, c, RelType::Calls, 1.0, "r2");
        fx.into_bytes()
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
        let mut fx = GraphFixture::new();
        let ids: Vec<u32> = ["a", "b", "c", "d"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let id = fx.func("src/x.ts", name);
                fx.span(id, (i as u32 * 2, 0, i as u32 * 2 + 1, 0));
                id
            })
            .collect();
        for i in 0..3 {
            fx.edge_with(ids[i], ids[i + 1], RelType::Calls, 1.0, &format!("r{i}"));
        }
        fx.into_bytes()
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("Calls".into()));
        });
    }

    #[test]
    fn exec_scalar_id_of_node() {
        with_two(|g| {
            let q = parse("MATCH (a:Function) RETURN id(a)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2);
            // First node's id is its index in graph.nodes.
            assert!(matches!(r.rows[0][0], Value::Int(_)));
        });
    }

    #[test]
    fn exec_scalar_labels_of_node() {
        with_two(|g| {
            let q = parse("MATCH (a:Function) WHERE a.name = 'caller' RETURN labels(a)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("Calls".into()));
            assert_eq!(r.rows[0][1], Value::Int(1));
        });
    }

    // -----------------------------------------------------------------------
    // WHERE-clause function calls — enabled by the single scalar dispatch
    // (`eval_scalar_funcall`) shared with RETURN and WITH group-key paths.
    // -----------------------------------------------------------------------

    #[test]
    fn exec_where_type_of_edge_filters_correctly() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) WHERE TYPE(r) = 'Calls' RETURN a.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
        });
    }

    #[test]
    fn exec_where_type_of_edge_mismatch_returns_empty() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) WHERE TYPE(r) = 'Imports' RETURN a.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 0);
        });
    }

    #[test]
    fn exec_where_labels_of_node_filters_correctly() {
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function) WHERE 'Function' IN labels(a) RETURN a.name ORDER BY a.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2);
        });
    }

    #[test]
    fn exec_where_aggregate_funcall_is_rejected() {
        with_two(|g| {
            let q = parse("MATCH (a:Function) WHERE count(a) > 1 RETURN a.name").unwrap();
            let err = execute(&q, g, None, Path::new(".")).unwrap_err();
            assert!(
                matches!(&err, CypherError::Exec { msg } if msg.contains("aggregate") && msg.contains("WHERE")),
                "expected aggregate-in-WHERE error, got {err:?}"
            );
        });
    }

    #[test]
    fn scalar_funcall_identical_across_where_return_and_with() {
        // Same TYPE(r) call through all three evaluators must agree —
        // regression guard for the unified `eval_scalar_funcall` dispatch.
        with_two(|g| {
            let where_q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) WHERE TYPE(r) = 'Calls' RETURN TYPE(r)",
            )
            .unwrap();
            let where_r = execute(&where_q, g, None, Path::new(".")).unwrap();

            let return_q =
                parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN TYPE(r)").unwrap();
            let return_r = execute(&return_q, g, None, Path::new(".")).unwrap();

            let with_q =
                parse("MATCH (a:Function)-[r:Calls]->(b:Function) WITH TYPE(r) AS t RETURN t")
                    .unwrap();
            let with_r = execute(&with_q, g, None, Path::new(".")).unwrap();

            assert_eq!(where_r.rows.len(), 1);
            assert_eq!(where_r.rows, return_r.rows);
            assert_eq!(where_r.rows, with_r.rows);
            assert_eq!(where_r.rows[0][0], Value::Str("Calls".into()));
        });
    }

    #[test]
    fn exec_where_funcall_on_with_alias_preserves_rows() {
        // Plain (non-aggregate) WITH rebinding clears node_vars/edge_vars and
        // stashes a NodeRef in `computed` instead — a WHERE funcall on the
        // aliased var must recover identity from there, not just node_vars.
        with_two(|g| {
            let q = parse("MATCH (a:Function) WITH a AS x WHERE ID(x) IS NOT NULL RETURN x.name")
                .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2, "both nodes must survive ID(x) IS NOT NULL");
        });
    }

    #[test]
    fn exec_where_funcall_on_aggregate_with_grouping_key_resolves() {
        // Aggregating WITH also clears node_vars/edge_vars; the grouping-key
        // var (`a`) must still resolve through `computed` for TYPE(r)/ID(a).
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) WITH a, r, COUNT(b) AS n WHERE TYPE(r) = 'Calls' RETURN a.name, n",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("caller".into()));
            assert_eq!(r.rows[0][1], Value::Int(1));
        });
    }

    #[test]
    fn exec_where_funcall_on_shadowing_with_alias_uses_new_binding() {
        // `WITH b AS a` shadows the surviving `a` name: computed["a"] becomes
        // the new binding (callee, idx 1), but the plain-rebind branch of
        // exec_with deliberately preserves the OLD node_vars["a"] (caller,
        // idx 0) unchanged for downstream MATCH traversal. `ID(a) = 1` only
        // stays true if the funcall resolves against the shadowed (new)
        // binding — a lookup-order bug that falls back to node_vars first
        // would evaluate `ID(a)` as 0, filtering the row out entirely, while
        // RETURN a.name (via prop_value, which already checks computed
        // first) would still project "callee" had the row survived — i.e.
        // the bug manifests as an incorrectly EMPTY result here, not a
        // wrong-but-present value.
        with_two(|g| {
            let q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) WITH b AS a WHERE ID(a) = 1 RETURN a.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                1,
                "WHERE ID(a) = 1 must resolve against the shadowed (new) binding, matching what RETURN a.name projects"
            );
            assert_eq!(r.rows[0][0], Value::Str("callee".into()));
        });
    }

    // -----------------------------------------------------------------------
    // AggregateKind exhaustiveness — every variant must produce a distinct,
    // working `Accumulator`. Guards the failure mode the old string-matched
    // `Accumulator::new` fallback (`_ => Counter(0)`) could hit silently.
    // -----------------------------------------------------------------------

    #[test]
    fn aggregate_kind_every_variant_builds_a_working_accumulator() {
        let kinds = [
            AggregateKind::Count,
            AggregateKind::Sum,
            AggregateKind::Avg,
            AggregateKind::Min,
            AggregateKind::Max,
            AggregateKind::Collect,
        ];
        for kind in kinds {
            let mut acc = Accumulator::new(kind, false);
            acc.feed(Value::Int(1), false);
            let out = acc.finalize();
            assert_ne!(
                out,
                Value::Null,
                "{kind:?} accumulator produced Null after feeding one row"
            );
        }
    }

    #[test]
    fn aggregate_kind_parse_matches_is_aggregate_fn_for_all_known_names() {
        for name in ["COUNT", "SUM", "AVG", "MIN", "MAX", "COLLECT"] {
            assert!(is_aggregate_fn(name));
            assert!(AggregateKind::parse(name).is_some());
        }
        assert!(!is_aggregate_fn("TYPE"));
        assert!(AggregateKind::parse("TYPE").is_none());
    }

    // -----------------------------------------------------------------------
    // OPTIONAL MATCH left-join
    // -----------------------------------------------------------------------

    /// Single isolated node with no outgoing edges.
    fn build_lone_node() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        let lone = fx.func("src/x.ts", "lone");
        fx.span(lone, (0, 0, 1, 0));
        fx.into_bytes()
    }

    fn with_lone<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_lone_node();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    #[test]
    fn cypher_exposes_line_props_one_based() {
        // Lone node has span (0,0,1,0): tree-sitter row 0..1 (0-based). Cypher
        // must surface 1-based line/startLine=1 and endLine=2, matching
        // impact/find output — never the raw 0-based span.
        with_lone(|g| {
            let q = parse("MATCH (n:Function) RETURN n.line, n.startLine, n.endLine").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Int(1), "n.line = span.0 + 1");
            assert_eq!(r.rows[0][1], Value::Int(1), "n.startLine = span.0 + 1");
            assert_eq!(r.rows[0][2], Value::Int(2), "n.endLine = span.2 + 1");
        });
    }

    #[test]
    fn exec_optional_match_returns_null_for_missing_hop() {
        // "lone" has no outgoing edges; OPTIONAL MATCH yields one row with b.name = null.
        with_lone(|g| {
            let q =
                parse("MATCH (a:Function) OPTIONAL MATCH (a)-[:Calls]->(b) RETURN a.name, b.name")
                    .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
        let mut fx = GraphFixture::new();
        let fan = fx.func("src/x.ts", "fan");
        fx.span(fan, (0, 0, 1, 0));
        let leaf_a = fx.func("src/x.ts", "leaf_a");
        fx.span(leaf_a, (2, 0, 3, 0));
        let leaf_b = fx.func("src/x.ts", "leaf_b");
        fx.span(leaf_b, (4, 0, 5, 0));
        fx.edge_with(fan, leaf_a, RelType::Calls, 0.8, "r1");
        fx.edge_with(fan, leaf_b, RelType::Calls, 0.6, "r2");
        fx.into_bytes()
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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

    #[test]
    fn exec_pattern_bound_second_node_is_constrained_not_rebound() {
        // f is pre-bound to leaf_a; when it reappears in the SECOND position of
        // a later pattern the hop must filter on that binding, not overwrite it.
        with_fan(|g| {
            let q = parse(
                "MATCH (f:Function {name: 'leaf_a'}) OPTIONAL MATCH (c)-[:Calls]->(f) RETURN f.name, c.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "expected exactly the fan→leaf_a edge");
            assert_eq!(r.rows[0][0], Value::Str("leaf_a".into()));
            assert_eq!(r.rows[0][1], Value::Str("fan".into()));
        });
    }

    // -----------------------------------------------------------------------
    // WHERE prop-equality pushdown into MATCH node patterns
    // -----------------------------------------------------------------------

    #[test]
    fn pushdown_moves_eq_conjunct_into_node_props() {
        let q = parse("MATCH (a:Function)-[:Calls]->(b) WHERE b.name = 'leaf_a' AND a.startLine > 0 RETURN a.name").unwrap();
        let rw = pushdown_where(&q).expect("b.name = literal must be pushed");
        let b_node = &rw.matches[0].patterns[0].nodes[1];
        assert!(
            b_node
                .props
                .iter()
                .any(|(k, l)| k == "name" && matches!(l, Literal::Str(s) if s == "leaf_a")),
            "pushed prop missing: {:?}",
            b_node.props
        );
        // The non-pushable comparison stays as residual WHERE.
        assert!(rw.where_.is_some(), "a.startLine > 0 must remain residual");
    }

    #[test]
    fn pushdown_results_identical_on_fan() {
        with_fan(|g| {
            let q = parse("MATCH (a:Function)-[:Calls]->(b) WHERE b.name = 'leaf_a' RETURN a.name")
                .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("fan".into()));
        });
    }

    #[test]
    fn pushdown_skips_var_introduced_by_optional_match() {
        // Pushing into an OPTIONAL pattern flips "row kept with null, then
        // WHERE-filtered" into "row kept" — left-join semantics must win.
        let q = parse(
            "MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) WHERE c.name = 'fan' RETURN f.name",
        )
        .unwrap();
        assert!(
            pushdown_where(&q).is_none(),
            "var first bound by OPTIONAL MATCH must not be pushed"
        );
    }

    #[test]
    fn pushdown_skips_uid_string_literal() {
        // BinOp eval has a dedicated diagnostic for n.uid = "string"; pushing
        // it down would silently return zero rows instead of that error.
        let q = parse("MATCH (n:Function) WHERE n.uid = '42' RETURN n.name").unwrap();
        assert!(pushdown_where(&q).is_none());
        with_fan(|g| {
            let err = execute(&q, g, None, Path::new(".")).unwrap_err();
            assert!(format!("{err}").contains("uid"), "diagnostic lost: {err}");
        });
    }

    #[test]
    fn pushdown_skips_content_prop() {
        // node_matches deliberately excludes "content" (file read).
        let q = parse("MATCH (n:Function) WHERE n.content = 'x' RETURN n.name").unwrap();
        assert!(pushdown_where(&q).is_none());
    }

    #[test]
    fn pushdown_recurses_into_union_branches() {
        let q = parse(
            "MATCH (a:Function) WHERE a.name = 'fan' RETURN a.name UNION MATCH (b:Method) WHERE b.name = 'err' RETURN b.name",
        )
        .unwrap();
        let rw = pushdown_where(&q).expect("both branches push");
        assert!(rw.where_.is_none(), "first branch fully pushed");
        let u = rw.union.as_ref().expect("union preserved");
        assert!(u.where_.is_none(), "union branch fully pushed");
        assert!(u.matches[0].patterns[0].nodes[0]
            .props
            .iter()
            .any(|(k, _)| k == "name"));
    }

    #[test]
    fn exec_optional_match_is_null_finds_orphans() {
        // The canonical orphan query: fan calls leaf_a/leaf_b; nobody calls fan.
        with_fan(|g| {
            let q = parse(
                "MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) WHERE c IS NULL RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "only the uncalled function survives");
            assert_eq!(r.rows[0][0], Value::Str("fan".into()));
        });
    }

    #[test]
    fn exec_optional_match_is_not_null_finds_called() {
        with_fan(|g| {
            let q = parse(
                "MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) WHERE c IS NOT NULL RETURN f.name ORDER BY f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2);
            assert_eq!(r.rows[0][0], Value::Str("leaf_a".into()));
            assert_eq!(r.rows[1][0], Value::Str("leaf_b".into()));
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "expected 1 row after skip+limit");
            assert_eq!(r.rows[0][0], Value::Str("leaf_a".into()));
        });
    }

    // -----------------------------------------------------------------------
    // UNION / UNION ALL
    // -----------------------------------------------------------------------

    /// Build a graph with one Function node and one Method node.
    fn build_func_and_method() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        let f = fx.func("src/x.ts", "my_func");
        fx.span(f, (0, 0, 1, 0));
        let m = fx.node(NodeKind::Method, "src/x.ts", "my_method");
        fx.span(m, (2, 0, 3, 0));
        fx.into_bytes()
    }

    fn with_func_and_method<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_func_and_method();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    #[test]
    fn exec_virtual_label_callable_in_match() {
        // :Callable must cover Function AND Method (and Constructor) — the
        // :Function-only orphan-query shape silently missed every Method.
        with_func_and_method(|g| {
            let q = parse("MATCH (n:Callable) RETURN n.name ORDER BY n.name").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2);
            assert_eq!(r.rows[0][0], Value::Str("my_func".into()));
            assert_eq!(r.rows[1][0], Value::Str("my_method".into()));
        });
    }

    #[test]
    fn exec_virtual_label_callable_in_where() {
        with_func_and_method(|g| {
            let q = parse("MATCH (n) WHERE n:Callable RETURN n.name").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2, "WHERE n:Callable must match both kinds");
        });
    }

    #[test]
    fn exec_union_concat() {
        // UNION concatenates results from two sub-queries.
        with_func_and_method(|g| {
            let q = parse("MATCH (a:Function) RETURN a.name UNION MATCH (b:Method) RETURN b.name")
                .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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

        let mut fx = GraphFixture::new();
        let hello = fx.func(rel_path, "hello");
        // span: start_row=0, start_col=0, end_row=2, end_col=1
        fx.span(hello, (0, 0, 2, 1));
        let bytes = fx.into_bytes();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();

        let q = parse("MATCH (a:Function) RETURN a.content").unwrap();
        let result = execute(&q, archived, None, dir.path()).unwrap();

        assert_eq!(result.columns, vec!["a.content"]);
        assert_eq!(result.rows.len(), 1);
        let content = match &result.rows[0][0] {
            Value::Str(s) => s.to_string(),
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
        use crate::graph::FunctionMeta;

        // visibility=private (2) encodes into bits 6-8: 2 << 6 = 0x80
        const PRIVATE_VISIBILITY: u16 = 2 << 6;

        let mut fx = GraphFixture::new();
        let mut ids = Vec::new();
        for (i, (kind, name)) in [
            (NodeKind::Function, "sync_fn"),
            (NodeKind::Function, "async_fn"),
            (NodeKind::Function, "test_fn"),
            (NodeKind::Function, "both_fn"),
            (NodeKind::Method, "override_method"),
            (NodeKind::Function, "py_route"),
        ]
        .into_iter()
        .enumerate()
        {
            let id = fx.node(kind, "src/x.ts", name);
            fx.span(id, (i as u32 * 2, 0, i as u32 * 2 + 1, 0));
            ids.push(id);
        }
        // ids[0] intentionally has no FunctionMeta (sparse-record absence).
        fx.function_meta(ids[1], FunctionMeta::FLAG_ASYNC, &[]);
        fx.function_meta(ids[2], FunctionMeta::FLAG_TEST, &[]);
        fx.function_meta(
            ids[3],
            FunctionMeta::FLAG_TEST | FunctionMeta::FLAG_ASYNC,
            &[],
        );
        fx.function_meta(ids[4], PRIVATE_VISIBILITY, &["@Override"]);
        fx.function_meta(ids[5], 0, &["app.get"]);
        fx.into_bytes()
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            let names: Vec<_> = r
                .rows
                .iter()
                .map(|row| match &row[0] {
                    Value::Str(s) => s.to_string(),
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            let names: Vec<_> = r
                .rows
                .iter()
                .map(|row| match &row[0] {
                    Value::Str(s) => s.to_string(),
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r_py = execute(&q_py, g, None, Path::new(".")).unwrap();
            assert_eq!(r_py.rows.len(), 1, "expected py_route for app.get");
            assert_eq!(r_py.rows[0][0], Value::Str("py_route".into()));

            // @Override stored as "@Override" but normalized to "Override"
            let q_java =
                parse("MATCH (m:Function|Method) WHERE 'Override' IN m.decorators RETURN m.name")
                    .unwrap();
            let r_java = execute(&q_java, g, None, Path::new(".")).unwrap();
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
            let r_raw = execute(&q_raw, g, None, Path::new(".")).unwrap();
            assert!(r_raw.rows.is_empty(), "@Override with @ should not match");
        });
    }

    // e) visibility = 2 returns only private nodes
    #[test]
    fn fm_visibility_private_filter() {
        with_fm(|g| {
            let q =
                parse("MATCH (f:Function|Method) WHERE f.visibility = 2 RETURN f.name").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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
            let r = execute(&q, g, None, Path::new(".")).unwrap();
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

    // Width invariant — pins the contract that every emitted row has
    // exactly `columns.len()` values across the representative projection
    // shapes (single-column / multi-column / aggregation). The CLI layer
    // (`cypher::build_payload`) relies on this to flatten single-column
    // rows to scalars without ambiguity.
    #[test]
    fn row_width_matches_columns_for_all_projection_shapes() {
        with_two(|g| {
            // single column
            let q = parse("MATCH (n:Function) RETURN n.name").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.columns.len(), 1);
            assert!(r.rows.iter().all(|row| row.len() == 1));

            // multi column
            let q =
                parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN a.name, b.name").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.columns.len(), 2);
            assert!(r.rows.iter().all(|row| row.len() == 2));

            // aggregation (COUNT) — 1 group, 1 column
            let q = parse("MATCH (n:Function) RETURN count(n)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(r.rows.iter().all(|row| row.len() == r.columns.len()));
        });
    }

    // -----------------------------------------------------------------------
    // Predicate pushdown: `<StringLiteral> IN <var>.decorators`
    // Fast path walks the archived rkyv slice directly (no Value::List alloc).
    // Tests verify: match, non-match, empty-list, and fallback paths.
    // -----------------------------------------------------------------------

    // g1) pushdown: matching literal returns Bool(true) via fast path
    #[test]
    fn pushdown_decorator_in_match_returns_true() {
        with_fm(|g| {
            let q =
                parse("MATCH (m:Function|Method) WHERE 'Override' IN m.decorators RETURN m.name")
                    .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("override_method".into()));
        });
    }

    // g2) pushdown: non-matching literal returns Bool(false) — node excluded
    #[test]
    fn pushdown_decorator_in_nonmatch_returns_false() {
        with_fm(|g| {
            let q = parse(
                "MATCH (m:Function|Method) WHERE 'NoSuchDecorator' IN m.decorators RETURN m.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(r.rows.is_empty(), "no node has 'NoSuchDecorator': {r:?}");
        });
    }

    // g3) pushdown: node with no FunctionMeta record (empty decorator list) returns false, not panic
    #[test]
    fn pushdown_decorator_in_empty_list_returns_false() {
        with_fm(|g| {
            // sync_fn (node 0) has no FunctionMeta → decorators absent → must be false
            let q = parse(
                "MATCH (f:Function) WHERE f.name = 'sync_fn' AND 'Override' IN f.decorators RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(r.rows.is_empty(), "sync_fn has no decorators: {r:?}");
        });
    }

    // g4) generic IN over a literal list (not a PropAccess) must fall through to generic path
    #[test]
    fn generic_in_literal_list_still_works() {
        with_two(|g| {
            let q = parse("MATCH (a:Function) WHERE a.name IN ['caller', 'callee'] RETURN a.name")
                .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 2, "both caller and callee must match: {r:?}");
        });
    }

    // g5) InCollection over a non-decorators prop (e.g. m.kind) must use generic path
    // This test uses the fan fixture and checks that a non-decorators collection
    // falls through correctly (produces empty result since Value::Str != Value::List).
    #[test]
    fn generic_in_collection_non_decorator_prop_falls_through() {
        with_fan(|g| {
            // m.kind is a Str, not a List — generic path returns false for every node,
            // so the WHERE eliminates all rows.
            let q = parse("MATCH (m:Function) WHERE 'Function' IN m.kind RETURN m.name").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            // m.kind evaluates to Value::Str("Function"), not Value::List — generic
            // InCollection arm returns false → zero rows pass the WHERE filter.
            assert!(
                r.rows.is_empty(),
                "non-list InCollection must return false: {r:?}"
            );
        });
    }

    // -----------------------------------------------------------------------
    // Accumulator semantics: empty input, NULL-skipping, collect order,
    // GROUP BY with multiple aggregates.
    // -----------------------------------------------------------------------

    #[test]
    fn agg_count_star_empty_bindings_returns_zero() {
        // No nodes of kind Class: COUNT(*) over empty binding set = 0 (not NULL).
        with_lone(|g| {
            let q = parse("MATCH (n:Class) RETURN COUNT(*)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Int(0));
        });
    }

    #[test]
    fn agg_count_expr_empty_bindings_returns_zero() {
        with_lone(|g| {
            let q = parse("MATCH (n:Class) RETURN COUNT(n.name)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Int(0));
        });
    }

    #[test]
    fn agg_min_max_empty_bindings_return_null() {
        // OpenCypher: MIN/MAX over empty set = NULL.
        with_lone(|g| {
            let q = parse("MATCH (n:Class) RETURN MIN(n.uid), MAX(n.uid)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Null, "MIN empty -> Null");
            assert_eq!(r.rows[0][1], Value::Null, "MAX empty -> Null");
        });
    }

    #[test]
    fn agg_avg_empty_bindings_returns_null() {
        with_lone(|g| {
            let q = parse("MATCH (n:Class) RETURN AVG(n.uid)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Null, "AVG empty -> Null");
        });
    }

    #[test]
    fn agg_count_star_single_binding() {
        // One node -> COUNT(*) = 1.
        with_lone(|g| {
            let q = parse("MATCH (n:Function) RETURN COUNT(*)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows[0][0], Value::Int(1));
        });
    }

    #[test]
    fn agg_count_star_many_bindings() {
        // Fan fixture has 3 Function nodes.
        with_fan(|g| {
            let q = parse("MATCH (n:Function) RETURN COUNT(*)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows[0][0], Value::Int(3));
        });
    }

    #[test]
    fn agg_collect_preserves_insertion_order() {
        // fan -Calls-> leaf_a (edge 0), fan -Calls-> leaf_b (edge 1).
        // Pattern traversal order must be preserved by COLLECT.
        with_fan(|g| {
            let q =
                parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN COLLECT(b.name)").unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            let list = match &r.rows[0][0] {
                Value::List(v) => v.clone(),
                other => panic!("expected List, got {other:?}"),
            };
            assert_eq!(list.len(), 2);
            assert_eq!(list[0], Value::Str("leaf_a".into()));
            assert_eq!(list[1], Value::Str("leaf_b".into()));
        });
    }

    #[test]
    fn agg_group_by_multiple_aggregates() {
        // fan->leaf_a (conf=0.8), fan->leaf_b (conf=0.6).
        // GROUP BY a.name with COUNT(*) and SUM(r.confidence) in same RETURN.
        with_fan(|g| {
            let q = parse(
                "MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, COUNT(*), SUM(r.confidence)",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0][0], Value::Str("fan".into()));
            assert_eq!(r.rows[0][1], Value::Int(2));
            assert!(
                matches!(r.rows[0][2], Value::Float(f) if (f - 1.4).abs() < 1e-6),
                "SUM should be ~1.4, got {:?}",
                r.rows[0][2]
            );
        });
    }

    #[test]
    fn dedup_rows_collapses_identical() {
        let mut rows = vec![
            vec![Value::Int(1), Value::Str("a".into())],
            vec![Value::Int(1), Value::Str("a".into())],
            vec![Value::Int(2), Value::Str("a".into())],
        ];
        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn dedup_rows_distinguishes_floats() {
        let mut rows = vec![
            vec![Value::Float(1.0)],
            vec![Value::Float(1.5)],
            vec![Value::Float(1.0)],
        ];
        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn dedup_rows_list_length_prefix_guard() {
        // ["a","b"] must not collide with ["ab"] — length-prefixing the
        // Str bytes is what prevents the boundary ambiguity.
        let mut rows = vec![
            vec![Value::List(vec![
                Value::Str("a".into()),
                Value::Str("b".into()),
            ])],
            vec![Value::List(vec![Value::Str("ab".into())])],
        ];
        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn dedup_rows_distinguishes_int_float_same_value() {
        // Int(1) and Float(1.0) carry different tags → distinct rows.
        let mut rows = vec![vec![Value::Int(1)], vec![Value::Float(1.0)]];
        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Inline node property map filtering  {key: value}
    // -----------------------------------------------------------------------

    /// MATCH (n {name:"caller"}) on the two-node fixture must return exactly
    /// 1 row — only the node whose name is "caller", not both nodes.
    #[test]
    fn exec_inline_prop_name_filters_correctly() {
        with_two(|g| {
            let q = parse(r#"MATCH (n {name:"caller"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                1,
                "inline prop filter must return only matching node, got {:?}",
                r.rows
            );
            assert_eq!(r.rows[0][0], Value::Str("caller".into()));
        });
    }

    /// MATCH (n {name:"callee"}) must return only the callee node.
    #[test]
    fn exec_inline_prop_name_other_value_filters_correctly() {
        with_two(|g| {
            let q = parse(r#"MATCH (n {name:"callee"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                1,
                "inline prop filter for callee must return 1 row, got {:?}",
                r.rows
            );
            assert_eq!(r.rows[0][0], Value::Str("callee".into()));
        });
    }

    /// MATCH (n {name:"nonexistent"}) must return 0 rows — no node matches.
    #[test]
    fn exec_inline_prop_name_nonexistent_returns_empty() {
        with_two(|g| {
            let q = parse(r#"MATCH (n {name:"nonexistent"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(
                r.rows.is_empty(),
                "inline prop filter for unknown name must return 0 rows, got {:?}",
                r.rows
            );
        });
    }

    /// MATCH (n:Function {name:"caller"}) — label + inline prop both applied.
    #[test]
    fn exec_inline_prop_with_label_filters_correctly() {
        with_two(|g| {
            let q = parse(r#"MATCH (n:Function {name:"caller"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                1,
                "label + inline prop filter must return 1 row, got {:?}",
                r.rows
            );
            assert_eq!(r.rows[0][0], Value::Str("caller".into()));
        });
    }

    /// `two_node_graph` as built — with the v10 kind CSR the real builder
    /// emits, exercising the `use_kind_csr = true` branch in `exec_pattern`.
    fn build_two_node_with_csr() -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(&two_node_graph())
            .unwrap()
            .to_vec()
    }

    fn with_two_csr<F: FnOnce(&crate::graph::ArchivedZeroCopyGraph)>(f: F) {
        let bytes = build_two_node_with_csr();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        f(archived);
    }

    /// CSR-path: MATCH (n:Function {name:"caller"}) must return 1 row, not 2.
    /// This exercises the `use_kind_csr = true` branch in exec_pattern.
    #[test]
    fn exec_inline_prop_with_csr_label_filters_correctly() {
        with_two_csr(|g| {
            // Verify CSR is actually enabled for this fixture
            assert!(
                !g.kind_offsets.is_empty(),
                "fixture must have CSR populated"
            );
            let q = parse(r#"MATCH (n:Function {name:"caller"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                1,
                "CSR path: inline prop filter must return only caller, got {:?}",
                r.rows
            );
            assert_eq!(r.rows[0][0], Value::Str("caller".into()));
        });
    }

    /// CSR-path: MATCH (n:Function {name:"nobody"}) must return 0 rows.
    #[test]
    fn exec_inline_prop_with_csr_nonexistent_returns_empty() {
        with_two_csr(|g| {
            let q = parse(r#"MATCH (n:Function {name:"nobody"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(
                r.rows.is_empty(),
                "CSR path: inline prop filter for unknown name must return 0 rows, got {:?}",
                r.rows
            );
        });
    }

    /// `MATCH (n {filePath:"src/x.ts"})` must return both nodes (both share the
    /// same file path in the two-node fixture).  Previously broken: `filePath`
    /// was not handled in node_matches, so the `_ => return false` catch-all
    /// made every node fail the match → 0 rows instead of 2.
    #[test]
    fn exec_inline_prop_file_path_filters_correctly() {
        with_two(|g| {
            let q = parse(r#"MATCH (n {filePath:"src/x.ts"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                2,
                "inline filePath filter must return both nodes with that path, got {:?}",
                r.rows
            );
        });
    }

    /// `MATCH (n {filePath:"nonexistent.ts"})` must return 0 rows.
    #[test]
    fn exec_inline_prop_file_path_nonexistent_returns_empty() {
        with_two(|g| {
            let q = parse(r#"MATCH (n {filePath:"nonexistent.ts"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(
                r.rows.is_empty(),
                "inline filePath filter for nonexistent path must return 0 rows, got {:?}",
                r.rows
            );
        });
    }

    /// MATCH (n {kind:"Function"}) must return both nodes (both are :Function).
    /// `kind` is already in the hot-3 — regression guard that it stays working
    /// after the general-property extension.
    #[test]
    fn exec_inline_prop_kind_filters_correctly() {
        with_two(|g| {
            let q = parse(r#"MATCH (n {kind:"Function"}) RETURN n.name"#).unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(
                r.rows.len(),
                2,
                "inline kind filter must return both Function nodes, got {:?}",
                r.rows
            );
        });
    }

    // -----------------------------------------------------------------------
    // EXISTS: variable-length range, multi-hop, and unbound shapes
    // -----------------------------------------------------------------------

    #[test]
    fn eval_exists_var_len_range_respects_depth_bounds() {
        // chain a→b→c: paths into c are b→c (depth 1) and a→b→c (depth 2).
        with_three(|g| {
            let q = parse(
                "MATCH (f:Function {name: 'c'}) WHERE EXISTS((x)-[:Calls*2..3]->(f)) RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "a→b→c is a depth-2 path into c");

            let q = parse(
                "MATCH (f:Function {name: 'c'}) WHERE EXISTS((x)-[:Calls*3..4]->(f)) RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(
                r.rows.is_empty(),
                "no depth-3 path into c — *3..4 must not match the depth-1/2 paths"
            );

            // b has only the depth-1 edge a→b: *2..2 must NOT degrade to single-hop.
            let q = parse(
                "MATCH (f:Function {name: 'b'}) WHERE EXISTS((x)-[:Calls*2..2]->(f)) RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(r.rows.is_empty(), "only a depth-1 edge reaches b");
        });
    }

    #[test]
    fn eval_exists_multi_hop_pattern_supported() {
        with_three(|g| {
            let q = parse(
                "MATCH (f:Function {name: 'c'}) WHERE EXISTS((x)-[:Calls]->(y)-[:Calls]->(f)) RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "a→b→c satisfies the two-hop pattern");

            let q = parse(
                "MATCH (f:Function {name: 'a'}) WHERE EXISTS((x)-[:Calls]->(y)-[:Calls]->(f)) RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(r.rows.is_empty(), "nothing two-hops into a");
        });
    }

    #[test]
    fn eval_exists_single_hop_all_unbound_scans_edges() {
        // EXISTS((x)-[:Calls]->(y)) with nothing bound = "does any Calls edge
        // exist" — answered by a short-circuit edge scan, not silent false.
        with_three(|g| {
            let q = parse(
                "MATCH (f:Function {name: 'a'}) WHERE EXISTS((x)-[:Calls]->(y)) RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert_eq!(r.rows.len(), 1, "the chain has Calls edges");

            let q = parse(
                "MATCH (f:Function {name: 'a'}) WHERE EXISTS((x)-[:Implements]->(y)) RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(r.rows.is_empty(), "the chain has no Implements edges");
        });
    }

    #[test]
    fn eval_exists_multi_hop_all_unbound_errors_not_silent_false() {
        with_three(|g| {
            let q = parse(
                "MATCH (f:Function) WHERE EXISTS((x)-[:Calls]->(y)-[:Calls]->(z)) RETURN f.name",
            )
            .unwrap();
            let e = execute(&q, g, None, Path::new(".")).unwrap_err();
            assert!(
                format!("{e}").contains("bound"),
                "must name the missing bound variable, got: {e}"
            );
        });
    }

    #[test]
    fn eval_exists_repeated_var_within_pattern_stays_consistent() {
        // (x)-->(y)-->(x) is a 2-cycle probe; the chain a→b→c has none, so the
        // repeated `f` must pin both ends to the SAME node and the predicate
        // must come back false — never true via two different endpoints.
        with_three(|g| {
            let q = parse(
                "MATCH (f:Function {name: 'a'}) WHERE EXISTS((f)-[:Calls]->(y)-[:Calls]->(f)) RETURN f.name",
            )
            .unwrap();
            let r = execute(&q, g, None, Path::new(".")).unwrap();
            assert!(r.rows.is_empty(), "chain has no cycle back to a");
        });
    }

    // -----------------------------------------------------------------------
    // IS [NOT] NULL evaluation (Task 3)
    // -----------------------------------------------------------------------

    #[test]
    fn eval_is_null_unbound_var_is_true() {
        with_two(|g| {
            // Binding has no var "b" → Var("b") → Value::Null → IS NULL = true.
            let binding = Binding::default();
            let expr = Expr::IsNull {
                expr: Box::new(Expr::Var("b".into())),
                negated: false,
            };
            let mut cache = ContentCache::new(PathBuf::from("."));
            let result = eval_expr(&expr, &binding, MergedGraph::new(g, None), &mut cache).unwrap();
            assert_eq!(
                result,
                Value::Bool(true),
                "unbound var IS NULL must be true"
            );
        });
    }

    #[test]
    fn eval_is_not_null_unbound_var_is_false() {
        with_two(|g| {
            let binding = Binding::default();
            let expr = Expr::IsNull {
                expr: Box::new(Expr::Var("b".into())),
                negated: true,
            };
            let mut cache = ContentCache::new(PathBuf::from("."));
            let result = eval_expr(&expr, &binding, MergedGraph::new(g, None), &mut cache).unwrap();
            assert_eq!(
                result,
                Value::Bool(false),
                "unbound var IS NOT NULL must be false"
            );
        });
    }

    #[test]
    fn eval_is_null_bound_var_is_false() {
        with_two(|g| {
            // Bind "n" to node 0 — Var("n") → Value::Str("caller") → not null.
            let mut binding = Binding::default();
            binding.node_vars.insert("n", 0);
            let expr = Expr::IsNull {
                expr: Box::new(Expr::Var("n".into())),
                negated: false,
            };
            let mut cache = ContentCache::new(PathBuf::from("."));
            let result = eval_expr(&expr, &binding, MergedGraph::new(g, None), &mut cache).unwrap();
            assert_eq!(
                result,
                Value::Bool(false),
                "bound var IS NULL must be false"
            );
        });
    }

    // -----------------------------------------------------------------------
    // EXISTS pattern evaluation (Task 4)
    // -----------------------------------------------------------------------

    /// NOT EXISTS((callee)-[:Calls]->()) with `callee` bound (node 1 has no
    /// outgoing Calls) → callee has no outgoing edge → pattern_exists = false
    /// → negated=true → EXISTS result = true.
    #[test]
    fn eval_exists_pattern_callee_no_outgoing_negated_true() {
        with_two(|g| {
            // callee is node 1 — it has no outgoing Calls edge.
            let mut binding = Binding::default();
            binding.node_vars.insert("callee", 1);
            let pattern = Pattern {
                nodes: vec![
                    NodePat {
                        var: Some("callee".into()),
                        kinds: vec![],
                        props: vec![],
                    },
                    NodePat {
                        var: None,
                        kinds: vec![],
                        props: vec![],
                    },
                ],
                rels: vec![RelPat {
                    var: None,
                    types: vec![RelType::Calls],
                    range: None,
                    dir: Direction::Out,
                }],
            };
            let expr = Expr::ExistsPattern {
                pattern,
                negated: true,
            };
            let mut cache = ContentCache::new(PathBuf::from("."));
            let result = eval_expr(&expr, &binding, MergedGraph::new(g, None), &mut cache).unwrap();
            assert_eq!(
                result,
                Value::Bool(true),
                "NOT EXISTS((callee)-[:Calls]->()) must be true (callee has no outgoing)"
            );
        });
    }

    /// EXISTS((caller)-[:Calls]->()) with `caller` bound (node 0 HAS an
    /// outgoing Calls edge) → pattern_exists = true → negated=false → true.
    #[test]
    fn eval_exists_pattern_caller_has_outgoing() {
        with_two(|g| {
            // caller is node 0 — it has an outgoing Calls edge to callee.
            let mut binding = Binding::default();
            binding.node_vars.insert("caller", 0);
            let pattern = Pattern {
                nodes: vec![
                    NodePat {
                        var: Some("caller".into()),
                        kinds: vec![],
                        props: vec![],
                    },
                    NodePat {
                        var: None,
                        kinds: vec![],
                        props: vec![],
                    },
                ],
                rels: vec![RelPat {
                    var: None,
                    types: vec![RelType::Calls],
                    range: None,
                    dir: Direction::Out,
                }],
            };
            let expr = Expr::ExistsPattern {
                pattern,
                negated: false,
            };
            let mut cache = ContentCache::new(PathBuf::from("."));
            let result = eval_expr(&expr, &binding, MergedGraph::new(g, None), &mut cache).unwrap();
            assert_eq!(
                result,
                Value::Bool(true),
                "EXISTS((caller)-[:Calls]->()) must be true"
            );
        });
    }

    /// Direction-inversion path: the bound endpoint is the SECOND node, so
    /// `pattern_exists` must invert the rel direction before walking. This is
    /// the actual execution shape of `NOT EXISTS((n)-[:Calls]->(callee))` where
    /// `callee` is the outer-bound var. Edge is node0 -[:Calls]-> node1, so
    /// node1 (callee) HAS an incoming Calls edge: with Out inverted to In the
    /// walk finds it → true. Without inversion, walking Out from node1 finds
    /// nothing → false, so this assertion fails if invert_dir is removed.
    #[test]
    fn eval_exists_pattern_second_node_anchor_inverts_dir() {
        with_two(|g| {
            // callee is node 1 — bound as the SECOND node of the pattern.
            let mut binding = Binding::default();
            binding.node_vars.insert("callee", 1);
            let pattern = Pattern {
                nodes: vec![
                    NodePat {
                        var: Some("n".into()),
                        kinds: vec![],
                        props: vec![],
                    },
                    NodePat {
                        var: Some("callee".into()),
                        kinds: vec![],
                        props: vec![],
                    },
                ],
                rels: vec![RelPat {
                    var: None,
                    types: vec![RelType::Calls],
                    range: None,
                    dir: Direction::Out,
                }],
            };
            let expr = Expr::ExistsPattern {
                pattern,
                negated: false,
            };
            let mut cache = ContentCache::new(PathBuf::from("."));
            let result = eval_expr(&expr, &binding, MergedGraph::new(g, None), &mut cache).unwrap();
            assert_eq!(
                result,
                Value::Bool(true),
                "EXISTS((n)-[:Calls]->(callee)) with callee second-node-bound must invert dir and find the incoming edge"
            );
        });
    }

    /// Orphan-shape negative on the inversion path: `caller` (node0) bound as
    /// the SECOND node — dir Out inverts to In, and node0 has NO incoming Calls
    /// edge → existence is false. This is the true-positive case for the real
    /// `NOT EXISTS((n)-[:Calls]->(caller))` orphan query (no incoming caller).
    #[test]
    fn eval_exists_pattern_second_node_anchor_no_incoming_is_false() {
        with_two(|g| {
            // caller is node 0 — bound as the SECOND node; it has no incoming edge.
            let mut binding = Binding::default();
            binding.node_vars.insert("caller", 0);
            let pattern = Pattern {
                nodes: vec![
                    NodePat {
                        var: Some("n".into()),
                        kinds: vec![],
                        props: vec![],
                    },
                    NodePat {
                        var: Some("caller".into()),
                        kinds: vec![],
                        props: vec![],
                    },
                ],
                rels: vec![RelPat {
                    var: None,
                    types: vec![RelType::Calls],
                    range: None,
                    dir: Direction::Out,
                }],
            };
            let expr = Expr::ExistsPattern {
                pattern,
                negated: true,
            };
            let mut cache = ContentCache::new(PathBuf::from("."));
            let result = eval_expr(&expr, &binding, MergedGraph::new(g, None), &mut cache).unwrap();
            assert_eq!(
                result,
                Value::Bool(true),
                "NOT EXISTS((n)-[:Calls]->(caller)) must be true: caller has no incoming Calls edge"
            );
        });
    }
}
