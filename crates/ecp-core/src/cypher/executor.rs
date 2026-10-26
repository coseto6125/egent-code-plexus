use crate::cypher::ast::*;
use crate::cypher::error::CypherError;
use crate::cypher::value::{QueryResult, Value};
use crate::graph::{ArchivedZeroCopyGraph, RelType};
use crate::session::{MergedGraph, MergedNode, OverlayView};
use compact_str::CompactString;
use rustc_hash::FxHashMap;
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

/// Cypher pipeline: MATCH → WHERE → WITH → RETURN → ORDER BY → DISTINCT →
/// SKIP/LIMIT → UNION. Each arrow below is one stage function — adding a
/// clause (e.g. UNWIND, which would rebind `bindings` between WITH and
/// RETURN the way `exec_with` does) means inserting one call in this list,
/// not splicing into a shared block.
fn execute_inner(
    query: &Query,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<QueryResult, CypherError> {
    let (columns, rows) = if has_aggregate(&query.return_.items)
        || query
            .with
            .as_ref()
            .is_some_and(|wc| has_aggregate(&wc.items))
    {
        stream_projection(query, graph, cache)?
    } else {
        let bindings = exec_matches(query, graph)?;
        let bindings = apply_where(query.where_.as_ref(), bindings, graph, cache)?;
        let bindings = match &query.with {
            Some(wc) => exec_with(wc, bindings, graph, cache)?,
            None => bindings,
        };
        project_return(
            &query.return_.items,
            &query.matches,
            &bindings,
            graph,
            cache,
        )?
    };

    let rows = apply_order_by(
        &query.order_by,
        &query.return_.items,
        &columns,
        rows,
        graph,
        cache,
    )?;
    let rows = apply_distinct(query.return_.distinct, rows);
    let rows = apply_skip_limit(query.skip, query.limit, rows);
    let rows = apply_union(
        query.union.as_deref(),
        query.union_all,
        &columns,
        rows,
        graph,
        cache,
    )?;

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

/// MATCH stage: fold each clause's patterns over the running binding set.
fn exec_matches(query: &Query, graph: MergedGraph<'_>) -> Result<Vec<Binding>, CypherError> {
    let mut bindings: Vec<Binding> = vec![Binding::default()];
    for mc in &query.matches {
        bindings = exec_match_clause(mc, &bindings, graph)?;
    }
    Ok(bindings)
}

/// WHERE stage: retain bindings whose predicate evaluates truthy.
fn apply_where(
    where_: Option<&Expr>,
    mut bindings: Vec<Binding>,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<Vec<Binding>, CypherError> {
    let Some(w) = where_ else {
        return Ok(bindings);
    };
    // Collect retain mask separately to avoid simultaneous &mut borrows.
    // Propagate eval errors (e.g. uid string-literal misuse) to the caller.
    let mask: Vec<bool> = bindings
        .iter()
        .map(|b| eval_expr(w, b, graph, cache).map(|v| value_truthy(&v)))
        .collect::<Result<_, _>>()?;
    let mut mask_iter = mask.into_iter();
    bindings.retain(|_| mask_iter.next().unwrap_or(false));
    Ok(bindings)
}

/// RETURN stage: expand bare-var items into concrete columns, then dispatch
/// to the aggregate or plain projection depending on what's in the list.
fn project_return(
    return_items: &[ReturnItem],
    matches: &[MatchClause],
    bindings: &[Binding],
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<(Vec<String>, Vec<Vec<Value>>), CypherError> {
    // Pre-expand bare Var RETURN items into concrete prop columns.
    // We use the first binding to infer whether each var is node/edge/computed-bound.
    let expanded_items: Vec<(String, ReturnExpr)> =
        expand_return_items(return_items, bindings.first(), matches)?;

    // Detect aggregation in expanded items. Scalar function calls (`type(r)`,
    // `id(n)`, `labels(n)`) are NOT aggregates and must not trigger the
    // group-by path; `plain_rows` projects them per-row via `eval_return_expr`.
    let has_agg = expanded_items.iter().any(|(_, e)| is_aggregate_expr(e));

    if has_agg {
        aggregate_rows(&expanded_items, bindings, graph, cache)
    } else {
        plain_rows(&expanded_items, bindings, graph, cache)
    }
}

/// No aggregation: simple row-by-row projection.
fn plain_rows(
    expanded_items: &[(String, ReturnExpr)],
    bindings: &[Binding],
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<(Vec<String>, Vec<Vec<Value>>), CypherError> {
    let columns: Vec<String> = expanded_items.iter().map(|(col, _)| col.clone()).collect();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for b in bindings {
        let mut row = Vec::new();
        for (_, expr) in expanded_items {
            row.push(eval_return_expr(expr, b, graph, cache)?);
        }
        rows.push(row);
    }
    Ok((columns, rows))
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Return,
    With,
}

/// Aggregates retain group keys and accumulator state, never input bindings.
struct Aggregation {
    items: Vec<(String, ReturnExpr)>,
    group_positions: Vec<usize>,
    aggregate_positions: Vec<usize>,
    specs: Vec<(bool, Option<Expr>, AggregateKind, bool)>,
    groups: Vec<(Vec<Value>, Vec<Accumulator>)>,
    key_index: FxHashMap<Vec<u8>, usize>,
    key_buffer: Vec<u8>,
    collapse: VarCollapse,
    stage: Stage,
}

impl Aggregation {
    fn new(items: Vec<(String, ReturnExpr)>, stage: Stage) -> Self {
        let mut group_positions = Vec::new();
        let mut aggregate_positions = Vec::new();
        let mut specs = Vec::new();
        for (i, (_, expr)) in items.iter().enumerate() {
            if is_aggregate_expr(expr) {
                let ReturnExpr::FunCall {
                    name,
                    distinct,
                    args,
                } = expr
                else {
                    unreachable!()
                };
                let kind = AggregateKind::parse(name).unwrap();
                let star = matches!(args.as_slice(), [Expr::Lit(Literal::Null)]);
                aggregate_positions.push(i);
                specs.push((star, args.first().cloned(), kind, *distinct));
            } else {
                group_positions.push(i);
            }
        }
        // Legacy ungrouped RETURN treats a single NULL/star argument as a row
        // count, even for a non-COUNT name. Preserve it through the same Counter.
        if stage == Stage::Return && group_positions.is_empty() && specs.len() == 1 && specs[0].0 {
            specs[0].2 = AggregateKind::Count;
            specs[0].3 = false;
        }
        Self {
            items,
            group_positions,
            aggregate_positions,
            specs,
            groups: Vec::new(),
            key_index: FxHashMap::default(),
            key_buffer: Vec::new(),
            collapse: match stage {
                Stage::Return => VarCollapse::ToStr,
                Stage::With => VarCollapse::ToRef,
            },
            stage,
        }
    }

    fn accumulators(&self) -> Vec<Accumulator> {
        self.specs
            .iter()
            .map(|(_, _, kind, distinct)| Accumulator::new(*kind, *distinct))
            .collect()
    }

    fn feed(
        &mut self,
        b: &Binding,
        graph: MergedGraph<'_>,
        cache: &mut ContentCache,
    ) -> Result<(), CypherError> {
        let slot = if self.group_positions.is_empty() {
            if self.groups.is_empty() {
                self.groups.push((Vec::new(), self.accumulators()));
            }
            0
        } else {
            self.key_buffer.clear();
            for &i in &self.group_positions {
                eval_return_expr_with(&self.items[i].1, b, graph, cache, self.collapse)
                    .write_aggregate_key(&mut self.key_buffer);
            }
            if let Some(&slot) = self.key_index.get(self.key_buffer.as_slice()) {
                slot
            } else {
                let slot = self.groups.len();
                let key_vals = self
                    .group_positions
                    .iter()
                    .map(|&i| {
                        eval_return_expr_with(&self.items[i].1, b, graph, cache, self.collapse)
                    })
                    .collect();
                self.groups.push((key_vals, self.accumulators()));
                self.key_index.insert(self.key_buffer.clone(), slot);
                slot
            }
        };
        for (accum, (star, arg, _, distinct)) in self.groups[slot].1.iter_mut().zip(&self.specs) {
            let value = if *star {
                Value::Null
            } else {
                eval_expr(arg.as_ref().unwrap(), b, graph, cache)?
            };
            // A bare node variable evaluates to its name, so DISTINCT would
            // merge same-named nodes. Key it by node identity instead.
            let node = match arg {
                Some(Expr::Var(var)) if *distinct && !b.computed.contains_key(var) => {
                    b.node_vars.get(var).copied()
                }
                _ => None,
            };
            accum.feed(value, *star, node);
        }
        Ok(())
    }

    fn finish_with(self) -> Vec<Binding> {
        let group_columns: Vec<_> = self
            .group_positions
            .iter()
            .map(|&i| self.items[i].0.clone())
            .collect();
        let agg_columns: Vec<_> = self
            .aggregate_positions
            .iter()
            .map(|&i| self.items[i].0.clone())
            .collect();
        self.groups
            .into_iter()
            .map(|(keys, accums)| {
                let mut computed: HashMap<_, _> = group_columns.iter().cloned().zip(keys).collect();
                // Existing WITH semantics insert aggregate aliases after group aliases.
                computed.extend(
                    agg_columns
                        .iter()
                        .cloned()
                        .zip(accums.into_iter().map(Accumulator::finalize)),
                );
                Binding {
                    computed,
                    ..Binding::default()
                }
            })
            .collect()
    }

    fn finish(mut self) -> (Vec<String>, Vec<Vec<Value>>) {
        let columns = self.items.iter().map(|(col, _)| col.clone()).collect();
        // WITH historically emits no group for empty input; RETURN emits a zero row.
        if self.groups.is_empty() && self.group_positions.is_empty() && self.stage == Stage::Return
        {
            self.groups.push((Vec::new(), self.accumulators()));
        }
        let rows = self
            .groups
            .into_iter()
            .map(|(keys, accums)| {
                let mut row = vec![Value::Null; self.items.len()];
                for (&i, value) in self.group_positions.iter().zip(keys) {
                    row[i] = value;
                }
                for (&i, accum) in self.aggregate_positions.iter().zip(accums) {
                    row[i] = accum.finalize();
                }
                row
            })
            .collect();
        (columns, rows)
    }
}

fn aggregate_rows(
    expanded_items: &[(String, ReturnExpr)],
    bindings: &[Binding],
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<(Vec<String>, Vec<Vec<Value>>), CypherError> {
    let mut agg = Aggregation::new(expanded_items.to_vec(), Stage::Return);
    for b in bindings {
        agg.feed(b, graph, cache)?;
    }
    Ok(agg.finish())
}

fn has_aggregate(items: &[ReturnItem]) -> bool {
    items.iter().any(|i| is_aggregate_expr(&i.expr))
}

fn is_aggregate_expr(expr: &ReturnExpr) -> bool {
    matches!(expr, ReturnExpr::FunCall { name, .. } if is_aggregate_fn(name))
}

fn stream_projection(
    query: &Query,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<(Vec<String>, Vec<Vec<Value>>), CypherError> {
    if let Some(wc) = &query.with {
        if has_aggregate(&wc.items) {
            let items = wc
                .items
                .iter()
                .map(|i| {
                    (
                        i.alias
                            .clone()
                            .unwrap_or_else(|| return_item_default_col(i)),
                        i.expr.clone(),
                    )
                })
                .collect();
            let mut agg = Aggregation::new(items, Stage::With);
            let mut aggregate_error = None;
            visit_matches(&query.matches, graph, &mut |b| {
                if passes_where(query.where_.as_ref(), b, graph, cache)?
                    && aggregate_error.is_none()
                {
                    aggregate_error = agg.feed(b, graph, cache).err();
                }
                Ok(())
            })?;
            if let Some(error) = aggregate_error {
                return Err(error);
            }
            let bindings = agg.finish_with();
            let bindings = apply_where(wc.where_.as_ref(), bindings, graph, cache)?;
            // Aggregation cleared the pattern variables; only WITH aliases remain.
            return project_return(&query.return_.items, &[], &bindings, graph, cache);
        }
    }
    let mut agg = None;
    let mut with_error = None;
    let mut aggregate_error = None;
    visit_matches(&query.matches, graph, &mut |b| {
        if !passes_where(query.where_.as_ref(), b, graph, cache)? {
            return Ok(());
        }
        let rebound;
        let b = if let Some(wc) = &query.with {
            if with_error.is_some() {
                return Ok(());
            }
            rebound = match rebind_one(wc, b, graph, cache) {
                Ok(row) => row,
                Err(error) => {
                    with_error = Some(error);
                    return Ok(());
                }
            };
            let Some(b) = rebound.as_ref() else {
                return Ok(());
            };
            b
        } else {
            b
        };
        // Complete WHERE stages before reporting a projection error.
        if aggregate_error.is_some() {
            return Ok(());
        }
        if agg.is_none() {
            match expand_return_items(&query.return_.items, Some(b), &query.matches) {
                Ok(items) => agg = Some(Aggregation::new(items, Stage::Return)),
                Err(error) => {
                    aggregate_error = Some(error);
                    return Ok(());
                }
            }
        }
        aggregate_error = agg.as_mut().unwrap().feed(b, graph, cache).err();
        Ok(())
    })?;
    if let Some(error) = with_error.or(aggregate_error) {
        return Err(error);
    }
    let agg = match agg {
        Some(agg) => agg,
        None => Aggregation::new(
            expand_return_items(&query.return_.items, None, &query.matches)?,
            Stage::Return,
        ),
    };
    Ok(agg.finish())
}

fn passes_where(
    where_: Option<&Expr>,
    b: &Binding,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<bool, CypherError> {
    match where_ {
        Some(expr) => eval_expr(expr, b, graph, cache).map(|v| value_truthy(&v)),
        None => Ok(true),
    }
}

/// Where an ORDER BY key reads its value from.
enum OrderKey {
    Column(usize),
    /// `v.prop` where column `v` holds a node reference (a WITH-computed node):
    /// the property is resolved through the node, row by row.
    NodeProp(usize, String),
}

/// Structural equality of two projection expressions. `Expr` has no
/// `PartialEq`, but `Debug` prints every field, so equal output is equal AST.
fn same_expr(a: &ReturnExpr, b: &ReturnExpr) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

/// Column position of each RETURN item, or `None` for all of them when the
/// widths do not add up to `columns`. A bare node or edge variable expands to
/// three columns, which shifts every later item, so positions come from
/// walking the items against the column names.
fn item_columns(items: &[ReturnItem], columns: &[String]) -> Vec<Option<usize>> {
    let mut pos = 0;
    let starts: Vec<usize> = items
        .iter()
        .map(|item| {
            let start = pos;
            pos += match (&item.expr, &item.alias) {
                (ReturnExpr::Var(v), None)
                    if columns.get(pos).is_some_and(|c| {
                        *c == format!("{v}.name") || *c == format!("{v}.rel_type")
                    }) =>
                {
                    3
                }
                _ => 1,
            };
            start
        })
        .collect();
    if pos == columns.len() {
        starts.into_iter().map(Some).collect()
    } else {
        vec![None; items.len()]
    }
}

/// Column an ORDER BY key sorts by. A RETURN item whose expression equals the
/// key gives its column even when it is aliased, so `RETURN f.name AS n ORDER
/// BY f.name` sorts. A key that is not in the projection is an error: the rows
/// no longer carry the value, and ignoring the key returned unsorted rows.
fn order_key(
    expr: &ReturnExpr,
    return_items: &[ReturnItem],
    item_cols: &[Option<usize>],
    col_index: &HashMap<String, usize>,
) -> Result<OrderKey, CypherError> {
    // A bare unaliased variable expands to several columns; it is resolved by
    // name below, as a node sorts by its name.
    let item_col = return_items
        .iter()
        .zip(item_cols)
        .find(|(item, _)| {
            !matches!((&item.expr, &item.alias), (ReturnExpr::Var(_), None))
                && same_expr(&item.expr, expr)
        })
        .and_then(|(_, col)| *col);
    if let Some(i) = item_col {
        return Ok(OrderKey::Column(i));
    }
    let exact = match expr {
        ReturnExpr::Prop(var, prop) => format!("{var}.{prop}"),
        ReturnExpr::Var(v) => v.clone(),
        ReturnExpr::Star => "*".into(),
        ReturnExpr::FunCall { name, .. } => format!("{name}(*)"),
        ReturnExpr::Subscript(e) => subscript_col_name(e),
    };
    if let Some(&i) = col_index.get(&exact) {
        return Ok(OrderKey::Column(i));
    }
    // A bare node variable evaluates to its name everywhere else in this engine.
    if let ReturnExpr::Var(v) = expr {
        if let Some(&i) = col_index.get(&format!("{v}.name")) {
            return Ok(OrderKey::Column(i));
        }
    }
    if let ReturnExpr::Prop(var, prop) = expr {
        if let Some(&i) = col_index.get(var.as_str()) {
            return Ok(OrderKey::NodeProp(i, prop.clone()));
        }
    }
    Err(CypherError::Semantic {
        msg: format!(
            "ORDER BY '{exact}' is not in the RETURN projection; add it to RETURN (or alias it) to sort by it"
        ),
    })
}

/// `prop` of the node a cell references. A null cell (an unmatched OPTIONAL
/// row) has a null property; any other cell is not a node, so ordering by its
/// property would be a silent no-op.
fn order_cell_prop(
    cell: &Value,
    prop: &str,
    key: &str,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<Value, CypherError> {
    match cell {
        Value::NodeRef { idx, .. } => Ok(node_prop_value(*idx, prop, graph, cache)),
        Value::Null => Ok(Value::Null),
        _ => Err(CypherError::Semantic {
            msg: format!(
                "ORDER BY '{key}': the column is not a node, so it has no property '{prop}'; return the property itself and sort by it"
            ),
        }),
    }
}

/// ORDER BY stage: no-op when the clause is absent.
fn apply_order_by(
    order_by: &[OrderItem],
    return_items: &[ReturnItem],
    columns: &[String],
    rows: Vec<Vec<Value>>,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<Vec<Vec<Value>>, CypherError> {
    if order_by.is_empty() {
        return Ok(rows);
    }
    // Pre-build column index once rather than scanning per comparison.
    let col_index: HashMap<String, usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| (c.clone(), i))
        .collect();
    let item_cols = item_columns(return_items, columns);
    let keys: Vec<OrderKey> = order_by
        .iter()
        .map(|oi| order_key(&oi.expr, return_items, &item_cols, &col_index))
        .collect::<Result<_, _>>()?;
    // A node-property key is resolved once per row, before the sort.
    let resolved: Vec<Option<Vec<Value>>> = keys
        .iter()
        .zip(order_by)
        .map(|(key, oi)| match (key, &oi.expr) {
            (OrderKey::NodeProp(col, prop), ReturnExpr::Prop(var, _)) => rows
                .iter()
                .map(|row| {
                    order_cell_prop(&row[*col], prop, &format!("{var}.{prop}"), graph, cache)
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            _ => Ok(None),
        })
        .collect::<Result<_, _>>()?;
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        for (k, (key, oi)) in keys.iter().zip(order_by).enumerate() {
            let ord = match (key, &resolved[k]) {
                (OrderKey::Column(c), _) => cmp_values(rows[a].get(*c), rows[b].get(*c)),
                (OrderKey::NodeProp(..), Some(values)) => {
                    cmp_values(Some(&values[a]), Some(&values[b]))
                }
                (OrderKey::NodeProp(..), None) => std::cmp::Ordering::Equal,
            };
            let ord = if oi.desc { ord.reverse() } else { ord };
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
    let mut slots: Vec<Option<Vec<Value>>> = rows.into_iter().map(Some).collect();
    Ok(order
        .into_iter()
        .map(|i| slots[i].take().expect("each row index appears once"))
        .collect())
}

/// DISTINCT stage.
fn apply_distinct(distinct: bool, mut rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    if distinct {
        dedup_rows(&mut rows);
    }
    rows
}

/// SKIP + LIMIT stage.
fn apply_skip_limit(
    skip: Option<u64>,
    limit: Option<u64>,
    rows: Vec<Vec<Value>>,
) -> Vec<Vec<Value>> {
    let skip = skip.unwrap_or(0) as usize;
    let mut rows = if skip > 0 {
        rows.into_iter().skip(skip).collect()
    } else {
        rows
    };
    if let Some(lim) = limit {
        rows.truncate(lim as usize);
    }
    rows
}

/// UNION / UNION ALL stage: recurse into the right-hand query and append.
fn apply_union(
    union_query: Option<&Query>,
    union_all: bool,
    columns: &[String],
    mut rows: Vec<Vec<Value>>,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<Vec<Vec<Value>>, CypherError> {
    let Some(union_query) = union_query else {
        return Ok(rows);
    };
    let right = execute_inner(union_query, graph, cache)?;
    if right.columns.len() != columns.len() {
        return Err(CypherError::Semantic {
            msg: "UNION column count mismatch".into(),
        });
    }
    // openCypher requires equal names; labelling the right rows with the left
    // names would present one branch's data under another branch's meaning.
    if right.columns != columns {
        return Err(CypherError::Semantic {
            msg: format!(
                "UNION branches return different column names: [{}] vs [{}]; give both branches the same names with AS, e.g. `RETURN a.name AS name UNION MATCH (b) RETURN b.name AS name`",
                columns.join(", "),
                right.columns.join(", ")
            ),
        });
    }
    rows.extend(right.rows);
    if !union_all {
        dedup_rows(&mut rows);
    }
    Ok(rows)
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
        // openCypher: null is the largest value, so ASC puts it last and the
        // `desc` reversal in `apply_order_by` puts it first.
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Greater,
        (_, Value::Null) => Less,
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

/// What a bare RETURN variable is bound to.
#[derive(Clone, Copy)]
enum VarShape {
    Computed,
    Node,
    Edge,
}

/// Shape of a variable that only an OPTIONAL clause introduces. An unmatched
/// optional row leaves it out of the binding, so the first row alone cannot
/// say whether it is a node or an edge; the pattern does. A variable that a
/// non-OPTIONAL clause binds is in every row and never reaches this lookup.
fn optional_var_shape(matches: &[MatchClause], name: &str) -> Option<VarShape> {
    matches
        .iter()
        .filter(|mc| mc.optional)
        .flat_map(|mc| &mc.patterns)
        .find_map(|pat| {
            if pat.nodes.iter().any(|n| n.var.as_deref() == Some(name)) {
                Some(VarShape::Node)
            } else if pat.rels.iter().any(|r| r.var.as_deref() == Some(name)) {
                Some(VarShape::Edge)
            } else {
                None
            }
        })
}

/// Expand RETURN items: replace bare `Var(name)` with 3 concrete prop columns
/// when `name` is node-bound (name/kind/filePath) or edge-bound (rel_type/confidence/reason).
/// Computed and unbound vars are kept as a single column. `matches` resolves a
/// variable that the first binding lacks only because its OPTIONAL clause did
/// not match that row; pass `&[]` where no pattern variable is in scope.
fn expand_return_items(
    items: &[ReturnItem],
    first_binding: Option<&Binding>,
    matches: &[MatchClause],
) -> Result<Vec<(String, ReturnExpr)>, CypherError> {
    let mut out: Vec<(String, ReturnExpr)> = Vec::new();
    for item in items {
        match &item.expr {
            ReturnExpr::Var(name) => {
                let shape = first_binding.and_then(|b| {
                    if b.computed.contains_key(name) {
                        Some(VarShape::Computed)
                    } else if b.node_vars.contains_key(name) {
                        Some(VarShape::Node)
                    } else if b.edge_vars.contains_key(name) {
                        Some(VarShape::Edge)
                    } else {
                        optional_var_shape(matches, name)
                    }
                });

                // If aliased with AS, treat as single column regardless of binding type.
                if let Some(col) = &item.alias {
                    out.push((col.clone(), item.expr.clone()));
                    continue;
                }
                match shape {
                    // Single computed column.
                    Some(VarShape::Computed) => out.push((name.clone(), item.expr.clone())),
                    // Expand node var into 3 columns: .name, .kind, .filePath
                    Some(VarShape::Node) => {
                        for prop in ["name", "kind", "filePath"] {
                            out.push((
                                format!("{name}.{prop}"),
                                ReturnExpr::Prop(name.clone(), prop.into()),
                            ));
                        }
                    }
                    // Expand edge var into 3 columns: .rel_type, .confidence, .reason
                    Some(VarShape::Edge) => {
                        for prop in ["rel_type", "confidence", "reason"] {
                            out.push((
                                format!("{name}.{prop}"),
                                ReturnExpr::Prop(name.clone(), prop.into()),
                            ));
                        }
                    }
                    // Bound binding exists but var is not in it — semantic error.
                    None if first_binding.is_some() => {
                        return Err(CypherError::Semantic {
                            msg: format!("unbound variable '{name}'"),
                        });
                    }
                    // No bindings at all (empty result set) — emit as-is.
                    None => out.push((name.clone(), item.expr.clone())),
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

/// How a bound node `Var` collapses when a `ReturnExpr::Var` is evaluated. The
/// two callers only ever disagree on this one axis — `Prop`/`Star`/`FunCall`
/// and edge variables are identical either way — so it is the sole parameter
/// distinguishing them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VarCollapse {
    /// Node vars resolve to their display name (`Value::Str`) — used by
    /// the plain RETURN projection path, matching legacy scalar semantics.
    ToStr,
    /// Node vars resolve to `Value::NodeRef`, preserving
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
            // Both collapses keep an edge as an EdgeRef: `RETURN r AS rel` is
            // the only way a bare edge variable reaches ToStr, and Null there
            // dropped the relationship the query asked for.
            if let Some(&eidx) = b.edge_vars.get(var) {
                if let Some(e) = graph.overlay_edge(eidx) {
                    return Value::EdgeRef {
                        src: e.source,
                        tgt: e.target,
                        rel_type: e.rel_type,
                        confidence: e.confidence,
                        reason: e.reason.to_string(),
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
            Value::Null
        }
        ReturnExpr::Prop(var, prop) => prop_value(var, prop, b, graph, cache),
        ReturnExpr::Star => Value::Null,
        ReturnExpr::FunCall { name, args, .. } => eval_scalar_funcall(name, args, b, graph),
        ReturnExpr::Subscript(e) => eval_subscript(e, b, graph, cache),
    }
}

/// Subscript projection never errors: a failed inner eval degrades to Null,
/// like the other scalar RETURN evaluators.
fn eval_subscript(
    e: &Expr,
    b: &Binding,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Value {
    eval_expr(e, b, graph, cache).unwrap_or(Value::Null)
}

/// Resolve a Cypher list index (negative counts from the end) to a position.
fn list_pos(i: i64, len: usize) -> Option<usize> {
    let pos = if i < 0 { i.checked_add(len as i64)? } else { i };
    usize::try_from(pos).ok().filter(|&p| p < len)
}

/// Clamp a slice bound (negative counts from the end) into `0..=len`.
fn slice_bound(v: Option<Value>, default: usize, len: usize) -> Option<usize> {
    match v {
        None => Some(default),
        Some(Value::Int(i)) => {
            let pos = if i < 0 {
                i.saturating_add(len as i64)
            } else {
                i
            };
            Some(pos.clamp(0, len as i64) as usize)
        }
        Some(_) => None,
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

/// Evaluate a ReturnItem's expression into a Value, preserving NodeRef/EdgeRef
/// so a plain WITH alias can still resolve node and edge properties.
fn eval_return_item_rich(
    item: &ReturnItem,
    b: &Binding,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Value {
    eval_return_expr_with(&item.expr, b, graph, cache, VarCollapse::ToRef)
}

/// Execute a plain WITH clause after aggregate WITH dispatch.
fn exec_with(
    wc: &WithClause,
    bindings: Vec<Binding>,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<Vec<Binding>, CypherError> {
    debug_assert!(!has_aggregate(&wc.items));
    let mut out = Vec::with_capacity(bindings.len());
    for b in &bindings {
        if let Some(row) = rebind_one(wc, b, graph, cache)? {
            out.push(row);
        }
    }
    Ok(out)
}

fn rebind_one(
    wc: &WithClause,
    b: &Binding,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<Option<Binding>, CypherError> {
    let computed = wc
        .items
        .iter()
        .map(|item| {
            let col = item
                .alias
                .clone()
                .unwrap_or_else(|| return_item_default_col(item));
            (col, eval_return_item_rich(item, b, graph, cache))
        })
        .collect();
    let rebound = Binding {
        node_vars: b.node_vars.clone(),
        edge_vars: b.edge_vars.clone(),
        computed,
    };
    Ok(passes_where(wc.where_.as_ref(), &rebound, graph, cache)?.then_some(rebound))
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
/// The parser refuses any other function name (`SCALAR_FUNCTIONS`), so the
/// `Value::Null` arms below are for a known function applied to an unbound or
/// wrong-kind variable (missing data), never for an unknown name. Supports the three
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
    // `computed["a"]`; `rebind_one` deliberately
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
    CounterDistinct(HashSet<Vec<u8>>, Vec<u8>),
    Summer {
        sum_i: i64,
        sum_f: f64,
        has_float: bool,
    },
    MinAccum(Option<Value>),
    MaxAccum(Option<Value>),
    Collector(Vec<Value>),
    CollectorDistinct(Vec<Value>, HashSet<Vec<u8>>, Vec<u8>),
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
                    Accumulator::CounterDistinct(HashSet::new(), Vec::new())
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
                    Accumulator::CollectorDistinct(Vec::new(), HashSet::new(), Vec::new())
                } else {
                    Accumulator::Collector(Vec::new())
                }
            }
            AggregateKind::Avg => Accumulator::Avg { sum: 0.0, count: 0 },
        }
    }

    /// Feed one row's projected value into this accumulator.
    /// `is_count_star`: if true this is `COUNT(*)` — value is ignored, row itself counts.
    /// `node`: the node index when the argument is a bare node variable; the
    /// DISTINCT accumulators key on it instead of on the displayed name.
    fn feed(&mut self, v: Value, is_count_star: bool, node: Option<u32>) {
        match self {
            Accumulator::Counter(n) => {
                if is_count_star || !matches!(v, Value::Null) {
                    *n += 1;
                }
            }
            Accumulator::CounterDistinct(seen, key) => {
                if !matches!(v, Value::Null) {
                    key.clear();
                    match node {
                        Some(idx) => Value::write_node_key(idx, key),
                        None => v.write_aggregate_key(key),
                    }
                    if !seen.contains(key.as_slice()) {
                        seen.insert(key.clone());
                    }
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
            Accumulator::CollectorDistinct(items, seen, key) => {
                if !matches!(v, Value::Null) {
                    key.clear();
                    match node {
                        Some(idx) => Value::write_node_key(idx, key),
                        None => v.write_aggregate_key(key),
                    }
                    if !seen.contains(key.as_slice()) {
                        seen.insert(key.clone());
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
            Accumulator::CounterDistinct(seen, _) => Value::Int(seen.len() as i64),
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
            Accumulator::CollectorDistinct(items, _, _) => Value::List(items),
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
    for b in prior {
        // Comma patterns are one join: each pattern extends the rows the
        // previous one produced, so a variable shared between them pins both
        // and unrelated patterns multiply (cartesian product).
        let joined = match mc.patterns.as_slice() {
            [only] => exec_pattern(only, b, graph)?,
            patterns => {
                let mut rows = vec![b.clone()];
                for pat in patterns {
                    let mut next = Vec::new();
                    for row in &rows {
                        next.extend(exec_pattern(pat, row, graph)?);
                    }
                    rows = next;
                }
                rows
            }
        };
        if mc.optional && joined.is_empty() {
            // Left-join: keep left binding, vars from this clause stay unset.
            out.push(b.clone());
        } else {
            out.extend(joined);
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

fn is_bound(np: &NodePat, base: &Binding) -> bool {
    np.var
        .as_deref()
        .is_some_and(|v| base.node_vars.contains_key(v))
}

/// First-node seeds for the who-calls shape `(a)-[:Calls]->(b {name:'x'})`:
/// one hop, and the only inline constraint is a `name` on the far end.
/// The full first-node scan clones a `Binding` per node before the hop
/// filters on `b` (vscode: 496k clones, 0.27 s / 429 MB). Here the seeds come
/// from the name-index hits, walked one hop backwards, so they are every
/// scanned node that can still produce a row. They come back in the scan's
/// own visit order, and both traversal paths still run their forward hop over
/// them. The rows, their order and parallel edges therefore stay the scan's.
///
/// `None` keeps the scan. An overlay is excluded: its virtual seeds and
/// merged edges have their own order. An empty name is excluded because the
/// name index skips tombstones, which the scan still matches.
fn far_end_name_seeds(
    pat: &Pattern,
    base: &Binding,
    graph: MergedGraph<'_>,
    use_kind_csr: bool,
) -> Option<Vec<u32>> {
    let [first_np, last_np] = pat.nodes.as_slice() else {
        return None;
    };
    let [rel] = pat.rels.as_slice() else {
        return None;
    };
    // `bound_start_pattern` (shared by `exec_pattern` and `visit_pattern`)
    // already reversed a pattern whose last node alone is bound, so only the
    // first node can arrive bound here.
    if graph.view().is_some()
        || graph.name_index.is_empty()
        || rel.range.is_some()
        || rel.dir == Direction::Both
        || !first_np.props.is_empty()
        || is_bound(first_np, base)
    {
        return None;
    }
    let name = inline_name(last_np).filter(|s| !s.is_empty())?;

    let back = RelPat {
        dir: invert_dir(rel.dir),
        ..rel.clone()
    };
    let mut seeds: Vec<u32> = Vec::new();
    for far in graph.nodes_by_name(name) {
        if !node_matches(far, last_np, graph) {
            continue;
        }
        walk_rel(far, &back, graph, |near, _| {
            if node_matches(near, first_np, graph) {
                seeds.push(near);
            }
        });
    }
    seeds.sort_unstable();
    seeds.dedup();
    if !use_kind_csr {
        return Some(seeds);
    }
    // The kind-CSR scan visits `first_np.kinds` in label order, and each
    // kind's nodes ascending: graph_assembly fills `kind_node_idx` in node
    // order. A kind listed twice is visited twice, so it is repeated here too.
    Some(
        first_np
            .kinds
            .iter()
            .flat_map(|&kind| {
                seeds
                    .iter()
                    .copied()
                    .filter(move |&i| graph.node(i).is_some_and(|m| m.kind() == kind))
            })
            .collect(),
    )
}

/// The inline `name: '<literal>'` prop of a node pattern, if any.
fn inline_name(np: &NodePat) -> Option<&str> {
    np.props
        .iter()
        .find_map(|(key, lit)| match (key.as_str(), lit) {
            ("name", Literal::Str(s)) => Some(s.as_str()),
            _ => None,
        })
}

/// The `name` literal that lets an unbound first node, not served by the kind
/// CSR, be seeded from the name index instead of `0..nodes.len()`. The caller
/// seeds from `name_candidates` (ascending like the full scan) and still runs
/// `base_visible` and `node_matches` on each, so every other inline prop is
/// still enforced.
///
/// `None` keeps the scan. An overlay is excluded: its virtual nodes are not in
/// the index and `seed_virtuals` appends them after the base seeds. An empty
/// name or an empty `name_index` still scans, inside `name_candidates`.
fn first_node_name<'a>(np: &'a NodePat, graph: MergedGraph<'_>) -> Option<&'a str> {
    if graph.view().is_some() {
        return None;
    }
    inline_name(np)
}

/// Feed `seed` the base-node indices an unbound first node can match: the
/// name-index hits when `first_node_name` allows, else every node.
fn for_each_first_node_seed(np: &NodePat, graph: MergedGraph<'_>, seed: impl FnMut(u32)) {
    match first_node_name(np, graph) {
        Some(name) => graph.name_candidates(name).for_each(seed),
        None => (0..graph.nodes.len() as u32).for_each(seed),
    }
}

// Each clause and fixed hop adds visitor frames. Deeper queries use the
// iterative materializing traversal to bound native stack usage.
const MAX_STREAM_DEPTH: usize = 64;

fn visit_matches(
    clauses: &[MatchClause],
    graph: MergedGraph<'_>,
    emit: &mut dyn FnMut(&Binding) -> Result<(), CypherError>,
) -> Result<(), CypherError> {
    let mut depth = 0;
    for mc in clauses {
        depth += mc
            .patterns
            .iter()
            .map(|pat| 1 + pat.rels.len())
            .sum::<usize>();
        if depth > MAX_STREAM_DEPTH {
            let mut bindings = vec![Binding::default()];
            for mc in clauses {
                bindings = exec_match_clause(mc, &bindings, graph)?;
            }
            for b in &bindings {
                emit(b)?;
            }
            return Ok(());
        }
    }
    visit_matches_inner(clauses, graph, emit)
}

/// Row order matches materialization, including OPTIONAL null rows.
fn visit_matches_inner(
    clauses: &[MatchClause],
    graph: MergedGraph<'_>,
    emit: &mut dyn FnMut(&Binding) -> Result<(), CypherError>,
) -> Result<(), CypherError> {
    let Some((mc, prior)) = clauses.split_last() else {
        return emit(&Binding::default());
    };
    visit_matches_inner(prior, graph, &mut |base: &Binding| {
        let mut matched = false;
        visit_patterns(&mc.patterns, base, graph, &mut |b: &Binding| {
            matched = true;
            emit(b)
        })?;
        if mc.optional && !matched {
            emit(base)?;
        }
        Ok(())
    })
}

/// One MATCH clause's comma patterns as a join: every row of a pattern is the
/// base of the next, mirroring `exec_match_clause`.
fn visit_patterns(
    patterns: &[Pattern],
    base: &Binding,
    graph: MergedGraph<'_>,
    emit: &mut dyn FnMut(&Binding) -> Result<(), CypherError>,
) -> Result<(), CypherError> {
    match patterns.split_first() {
        None => emit(base),
        Some((pat, rest)) => visit_pattern(pat, base, graph, &mut |b| {
            visit_patterns(rest, b, graph, emit)
        }),
    }
}

#[inline]
fn bound_start_pattern(pat: &Pattern, base: &Binding) -> Option<Pattern> {
    (pat.nodes.len() > 1
        && !is_bound(&pat.nodes[0], base)
        && is_bound(&pat.nodes[pat.nodes.len() - 1], base))
    .then(|| invert_pattern(pat))
}

#[inline]
fn for_each_pattern_seed(
    pat: &Pattern,
    base: &Binding,
    graph: MergedGraph<'_>,
    mut seed: impl FnMut(u32),
) {
    let first = &pat.nodes[0];
    let use_kind_csr = !graph.kind_offsets.is_empty()
        && !first.kinds.is_empty()
        && first
            .kinds
            .iter()
            .all(|k| graph.kind_offsets.len() > k.as_index() + 1);
    if let Some(seeds) = far_end_name_seeds(pat, base, graph, use_kind_csr) {
        seeds.into_iter().for_each(seed);
    } else if let Some(&idx) = first.var.as_ref().and_then(|v| base.node_vars.get(v)) {
        if node_matches(idx, first, graph) {
            seed(idx);
        }
    } else {
        if use_kind_csr {
            for &kind in &first.kinds {
                let k = kind.as_index();
                let start = graph.kind_offsets[k].to_native() as usize;
                let end = graph.kind_offsets[k + 1].to_native() as usize;
                for raw in &graph.kind_node_idx[start..end] {
                    let idx = raw.to_native();
                    if graph.base_visible(idx) && node_matches(idx, first, graph) {
                        seed(idx);
                    }
                }
            }
        } else {
            for_each_first_node_seed(first, graph, |idx| {
                if graph.base_visible(idx) && node_matches(idx, first, graph) {
                    seed(idx);
                }
            });
        }
        seed_virtuals(graph, first, seed);
    }
}

fn visit_pattern(
    pat: &Pattern,
    base: &Binding,
    graph: MergedGraph<'_>,
    emit: &mut dyn FnMut(&Binding) -> Result<(), CypherError>,
) -> Result<(), CypherError> {
    #[cfg(test)]
    tests::PATTERN_VISITS.with(|visits| visits.set(visits.get() + 1));
    if let Some(reversed) = bound_start_pattern(pat, base) {
        return visit_pattern(&reversed, base, graph, emit);
    }
    let first = &pat.nodes[0];
    let mut binding = base.clone();
    let mut result = Ok(());
    for_each_pattern_seed(pat, base, graph, |idx| {
        if result.is_err() {
            return;
        }
        if let Some(var) = &first.var {
            binding.node_vars.insert(var, idx);
        }
        result = visit_hops(pat, 0, &binding, idx, graph, emit);
    });
    result
}

fn visit_hops(
    pat: &Pattern,
    hop: usize,
    base: &Binding,
    current: u32,
    graph: MergedGraph<'_>,
    emit: &mut dyn FnMut(&Binding) -> Result<(), CypherError>,
) -> Result<(), CypherError> {
    let Some(rel) = pat.rels.get(hop) else {
        return emit(base);
    };
    let next = &pat.nodes[hop + 1];
    let mut binding = base.clone();
    let mut result = Ok(());
    let mut step = |target, edge: Option<u32>| {
        if result.is_err() || !node_matches(target, next, graph) {
            return;
        }
        if let Some(var) = &next.var {
            if base
                .node_vars
                .get(var)
                .is_some_and(|&pinned| pinned != target)
            {
                return;
            }
            binding.node_vars.insert(var, target);
        }
        if let Some(var) = &rel.var {
            // Zero-length BFS retains the incoming edge binding, if any.
            // Restore it after another result has installed a nonempty path.
            if let Some(edge) = edge {
                binding.edge_vars.insert(var, edge);
            } else {
                binding.edge_vars = base.edge_vars.clone();
            }
        }
        result = visit_hops(pat, hop + 1, &binding, target, graph, emit);
    };
    match rel.range {
        Some((min, max)) => {
            for (target, edge) in bfs_var_len(current, rel, graph, min, max) {
                step(target, edge);
            }
        }
        None => walk_rel(current, rel, graph, |target, edge| step(target, Some(edge))),
    }
    result
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
    if let Some(reversed) = bound_start_pattern(pat, base) {
        return exec_pattern(&reversed, base, graph);
    }

    let mut frontier: Vec<(Binding, u32)> = Vec::new();
    for_each_pattern_seed(pat, base, graph, |idx| {
        let mut binding = base.clone();
        if let Some(var) = &pat.nodes[0].var {
            binding.node_vars.insert(var, idx);
        }
        frontier.push((binding, idx));
    });

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

/// BFS for variable-length relationships `*min..max`, `min <= 1` (the parser
/// refuses a larger minimum). Returns `(target_node_idx, last_edge_idx_option)`
/// pairs reachable within the depth range. For a node other than `start`, a
/// shortest walk is a path, so reachability equals "a trail exists". `start`
/// itself needs a closed trail, checked by `closed_trail_exists`.
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
    if min == 1 && closed_trail_exists(start, rel, graph, max) {
        out.push((start, None));
    }
    out
}

/// Is there a trail (no relationship repeated) of 1..=`max` hops from `start`
/// back to `start`? A node-visited BFS never revisits `start`, so `*1..n`
/// would miss a cycle or a self-loop. Neither direction enumerates trails: a
/// closed trail through `start` exists exactly when `start` lies on a simple
/// cycle of at most `max` hops (a closed trail splits into edge-disjoint
/// simple cycles, and `start` is on one), so a shortest-path test is exact and
/// enumeration cost (degree^(k-2) per start row) is avoided.
fn closed_trail_exists(start: u32, rel: &RelPat, graph: MergedGraph<'_>, max: u32) -> bool {
    if max == 0 {
        return false;
    }
    if rel.dir == Direction::Both {
        undirected_cycle_exists(start, rel, graph, max)
    } else {
        directed_cycle_exists(start, rel, graph, max)
    }
}

/// Directed hops: the shortest closed walk through `start` is a simple cycle,
/// hence a trail. One reverse BFS gives every node's distance back to `start`
/// (`back`, up to `max - 1`); a self-loop is `start` itself at distance 0.
fn directed_cycle_exists(start: u32, rel: &RelPat, graph: MergedGraph<'_>, max: u32) -> bool {
    let reverse = RelPat {
        dir: invert_dir(rel.dir),
        ..rel.clone()
    };
    let mut back: HashSet<u32> = HashSet::from([start]);
    let mut frontier = vec![start];
    for _ in 1..max {
        let mut next = Vec::new();
        for &n in &frontier {
            walk_rel(n, &reverse, graph, |src, _| {
                if back.insert(src) {
                    next.push(src);
                }
            });
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    let mut closes = false;
    walk_rel(start, rel, graph, |tgt, _| closes |= back.contains(&tgt));
    closes
}

/// Undirected hops: for each relationship `e = (start, t)`, `start` is on a
/// cycle through `e` of at most `max` hops iff `t` reaches `start` in at most
/// `max - 1` hops without `e`. One BFS per incident relationship, so the cost
/// is O(degree * (V + E)), never exponential in `max`.
fn undirected_cycle_exists(start: u32, rel: &RelPat, graph: MergedGraph<'_>, max: u32) -> bool {
    let mut incident = Vec::new();
    walk_rel(start, rel, graph, |tgt, edge| incident.push((tgt, edge)));
    incident.into_iter().any(|(tgt, edge)| {
        tgt == start || reaches_without_edge(tgt, start, edge, rel, graph, max - 1)
    })
}

/// BFS from `from` to `target` within `hops` hops, never crossing `excluded`.
fn reaches_without_edge(
    from: u32,
    target: u32,
    excluded: u32,
    rel: &RelPat,
    graph: MergedGraph<'_>,
    hops: u32,
) -> bool {
    let mut seen: HashSet<u32> = HashSet::from([from]);
    let mut frontier = vec![from];
    for _ in 0..hops {
        let mut next = Vec::new();
        let mut found = false;
        for &n in &frontier {
            walk_rel(n, rel, graph, |tgt, edge| {
                if edge == excluded {
                    return;
                }
                found |= tgt == target;
                if seen.insert(tgt) {
                    next.push(tgt);
                }
            });
        }
        if found {
            return true;
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    false
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
    let fm = MetaSource::of(graph, &m, node_idx);
    match prop {
        "filePath" => Value::Str(m.file_path(&graph).unwrap_or("").into()),
        "ownerClass" => match graph.display_owner(node_idx) {
            Some(oc) => Value::Str(oc.as_ref().into()),
            None => Value::Null,
        },
        "line" | "startLine" => Value::Int(m.start_line() as i64),
        "endLine" => Value::Int(m.end_line() as i64),
        "is_test" | "isTest" => Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_TEST)),
        "is_async" | "isAsync" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_ASYNC))
        }
        "is_static" | "isStatic" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_STATIC))
        }
        "is_abstract" | "isAbstract" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_ABSTRACT))
        }
        "is_generator" | "isGenerator" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_GENERATOR))
        }
        "is_extern" | "isExtern" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_EXTERN))
        }
        "visibility" => Value::Int(fm.visibility(graph) as i64),
        // "content" excluded: requires file I/O; use WHERE n.content = … instead.
        // "decorators" is a list — inline-map equality against a list literal is
        // an edge case handled by literal_matches_value's List arm.
        "decorators" => fm
            .decorators
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
            // so DISTINCT keys retain edge identity.
            if let Some(&eidx) = b.edge_vars.get(var) {
                if let Some(e) = graph.overlay_edge(eidx) {
                    return Ok(Value::EdgeRef {
                        src: e.source,
                        tgt: e.target,
                        rel_type: e.rel_type,
                        confidence: e.confidence,
                        reason: e.reason.to_string(),
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
                    // The decorators of a virtual node are its base twin's,
                    // as the generic path reads them (`MetaSource`).
                    let base = graph.node(idx).and_then(|m| m.meta_idx(idx));
                    return Ok(Value::Bool(
                        base.is_some_and(|i| archived_decorator_contains(graph, i, needle)),
                    ));
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
        Index(base, idx) => {
            let idx = eval_expr(idx, b, graph, cache)?;
            // `n['name']` reads a property, as `n.name` does.
            if let (Expr::Var(var), Value::Str(prop)) = (base.as_ref(), &idx) {
                return Ok(prop_value(var, prop, b, graph, cache));
            }
            let (Value::List(items), Value::Int(i)) = (eval_expr(base, b, graph, cache)?, idx)
            else {
                return Ok(Value::Null);
            };
            Ok(list_pos(i, items.len()).map_or(Value::Null, |p| items[p].clone()))
        }
        Slice { base, from, to } => {
            let Value::List(items) = eval_expr(base, b, graph, cache)? else {
                return Ok(Value::Null);
            };
            let len = items.len();
            let mut bound = |e: &Option<Box<Expr>>, default| match e {
                None => Ok(slice_bound(None, default, len)),
                Some(e) => {
                    eval_expr(e, b, graph, cache).map(|v| slice_bound(Some(v), default, len))
                }
            };
            let (Some(lo), Some(hi)) = (bound(from, 0)?, bound(to, len)?) else {
                return Ok(Value::Null);
            };
            Ok(Value::List(
                items.get(lo..hi.max(lo)).unwrap_or(&[]).to_vec(),
            ))
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
        // `pinned_target`: the far endpoint is bound too, so the edge must reach
        // exactly that node, not just any node the far pattern accepts.
        let (anchor, anchor_pat, target_pat, pinned_target, dir) = match (bound0, bound1) {
            (Some(idx), far) => (idx, n0, n1, far, rel.dir),
            (None, Some(idx)) => (idx, n1, n0, None, invert_dir(rel.dir)),
            (None, None) => {
                return Ok(edge_scan_exists(n0, n1, rel, graph));
            }
        };
        if !node_matches(anchor, anchor_pat, graph) {
            return Ok(false);
        }
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
            if pinned_target.is_none_or(|p| p == tgt) && node_matches(tgt, target_pat, graph) {
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
    if pat.nodes.len() > 1
        && !is_bound(&pat.nodes[0], base)
        && is_bound(&pat.nodes[pat.nodes.len() - 1], base)
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
                "reason" => Value::Str(e.reason.into()),
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
    let fm = MetaSource::of(graph, &m, node_idx);
    match prop {
        "name" => Value::Str(m.name(&graph).into()),
        // u64 uid stored as i64 bits — no allocation per row.
        "uid" => Value::Int(m.uid() as i64),
        "ownerClass" => match graph.display_owner(node_idx) {
            Some(oc) => Value::Str(oc.as_ref().into()),
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
        "is_test" | "isTest" => Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_TEST)),
        "is_async" | "isAsync" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_ASYNC))
        }
        "is_static" | "isStatic" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_STATIC))
        }
        "is_abstract" | "isAbstract" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_ABSTRACT))
        }
        "is_generator" | "isGenerator" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_GENERATOR))
        }
        "is_extern" | "isExtern" => {
            Value::Bool(fm.flag(graph, crate::graph::FunctionMeta::FLAG_EXTERN))
        }
        "visibility" => Value::Int(fm.visibility(graph) as i64),
        "decorators" => fm
            .decorators
            .map(|i| archived_fm_decorators(graph, i))
            .unwrap_or(Value::List(Vec::new())),
        _ => Value::Null,
    }
}

/// Where a node's function meta is read. A base node reads its archived
/// record. A virtual node reads the flags its fragment paired
/// ([`crate::session::OverlayView::function_meta`]), as a reindex would
/// store them: none paired means the sparse defaults, never its base twin's
/// stale flags. A fragment carries no decorators, so a virtual node's are
/// its base twin's (`MergedNode::meta_idx`), and a brand-new one has none.
#[derive(Clone, Copy)]
struct MetaSource {
    flags: FlagsAt,
    /// The BASE index whose archived decorators the node reports.
    decorators: Option<u32>,
}

#[derive(Clone, Copy)]
enum FlagsAt {
    Base(u32),
    Virtual(Option<u16>),
}

impl MetaSource {
    fn of(graph: MergedGraph<'_>, node: &MergedNode<'_>, idx: u32) -> Self {
        let flags = match node {
            MergedNode::Base(_) => FlagsAt::Base(idx),
            MergedNode::Virtual(_) => FlagsAt::Virtual(
                graph
                    .view()
                    .and_then(|view| view.function_meta(idx))
                    .map(|meta| meta.flags),
            ),
        };
        Self {
            flags,
            decorators: node.meta_idx(idx),
        }
    }

    /// Whether the meta has `flag`; false without a meta.
    fn flag(self, graph: MergedGraph<'_>, flag: u16) -> bool {
        match self.flags {
            FlagsAt::Base(idx) => archived_fm_flag(graph, idx, flag),
            FlagsAt::Virtual(flags) => flags.is_some_and(|flags| flags & flag != 0),
        }
    }

    /// The 3-bit visibility code; 0 (public) without a meta.
    fn visibility(self, graph: MergedGraph<'_>) -> u8 {
        match self.flags {
            FlagsAt::Base(idx) => archived_fm_visibility(graph, idx),
            FlagsAt::Virtual(flags) => flags.map_or(0, |flags| ((flags >> 6) & 0b111) as u8),
        }
    }
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
        ReturnExpr::Subscript(e) => subscript_col_name(e),
    }
}

/// Default column label for a subscripted projection: `labels(*)[]` style,
/// built from the innermost base so unaliased columns stay readable.
fn subscript_col_name(e: &Expr) -> String {
    // Two subscripts of one base need two names: ORDER BY and the reader
    // tell the columns apart by name.
    let bound = |e: &Expr| match e {
        Expr::Lit(Literal::Int(i)) => i.to_string(),
        Expr::Lit(Literal::Str(s)) => format!("'{s}'"),
        _ => "?".into(),
    };
    match e {
        Expr::Index(base, idx) => format!("{}[{}]", subscript_col_name(base), bound(idx)),
        Expr::Slice { base, from, to } => {
            let side = |e: &Option<Box<Expr>>| e.as_deref().map(bound).unwrap_or_default();
            format!("{}[{}..{}]", subscript_col_name(base), side(from), side(to))
        }
        Expr::Var(v) => v.clone(),
        Expr::Prop(v, p) => format!("{v}.{p}"),
        Expr::FunCall { name, .. } => format!("{name}(*)"),
        _ => "expr".into(),
    }
}

#[cfg(test)]
mod tests;
