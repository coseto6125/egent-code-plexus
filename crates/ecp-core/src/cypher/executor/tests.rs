use super::*;
use crate::session::{OverlayFileInput, OverlaySymbol};
use std::cell::Cell;

thread_local! {
    pub(super) static PATTERN_VISITS: Cell<usize> = const { Cell::new(0) };
}

fn execute_materialising(
    query: &Query,
    graph: MergedGraph<'_>,
    cache: &mut ContentCache,
) -> Result<QueryResult, CypherError> {
    let bindings = exec_matches(query, graph)?;
    let bindings = apply_where(query.where_.as_ref(), bindings, graph, cache)?;
    let bindings = match &query.with {
        Some(wc) if has_aggregate(&wc.items) => {
            let items = wc
                .items
                .iter()
                .map(|item| {
                    (
                        item.alias
                            .clone()
                            .unwrap_or_else(|| return_item_default_col(item)),
                        item.expr.clone(),
                    )
                })
                .collect();
            let mut agg = Aggregation::new(items, Stage::With);
            for binding in &bindings {
                agg.feed(binding, graph, cache)?;
            }
            apply_where(wc.where_.as_ref(), agg.finish_with(), graph, cache)?
        }
        Some(wc) => exec_with(wc, bindings, graph, cache)?,
        None => bindings,
    };

    // An aggregating WITH clears the pattern variables, as in `stream_projection`.
    let matches: &[MatchClause] = match &query.with {
        Some(wc) if has_aggregate(&wc.items) => &[],
        _ => &query.matches,
    };
    let (columns, rows) = project_return(&query.return_.items, matches, &bindings, graph, cache)?;

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

#[test]
fn test_execute_later_comma_patterns_visit_prefix_once() {
    let mut fx = GraphFixture::new();
    for i in 0..2_000 {
        fx.func("src/prefix.rs", &format!("f{i}"));
    }
    let bytes = fx.into_bytes();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let query = format!(
        "MATCH (a:Function {{startLine:0}}) {} RETURN count(*)",
        "MATCH (b), (c) ".repeat(10)
    );
    PATTERN_VISITS.with(|visits| visits.set(0));
    let result = execute(&parse(&query).unwrap(), graph, None, Path::new(".")).unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(0)]]);
    assert_eq!(PATTERN_VISITS.with(Cell::get), 1);
}

#[test]
fn test_execute_100000_clauses_returns_one_without_stack_overflow() {
    const CHILD: &str = "ECP_TEST_DEEP_MATCH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cypher::executor::tests::test_execute_100000_clauses_returns_one_without_stack_overflow", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut fx = GraphFixture::new();
    fx.func("src/deep.rs", "only");
    let bytes = fx.into_bytes();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let query = format!("{} RETURN count(*)", "MATCH (:Function) ".repeat(100_000));
    let result = execute(&parse(&query).unwrap(), graph, None, Path::new(".")).unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

#[test]
fn test_execute_deep_fixed_hops_use_materialising_traversal() {
    let mut fx = GraphFixture::new();
    let node = fx.func("src/deep.rs", "only");
    fx.edge(node, node, RelType::Calls);
    let bytes = fx.into_bytes();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let text = format!(
        "MATCH (a:Function){} RETURN count(*)",
        "-[:Calls]->()".repeat(1_000)
    );
    PATTERN_VISITS.with(|visits| visits.set(0));
    let result = execute(&parse(&text).unwrap(), graph, None, Path::new(".")).unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
    assert_eq!(PATTERN_VISITS.with(Cell::get), 0);
}

#[test]
fn test_execute_aggregate_keys_preserve_parallel_edge_reasons() {
    let mut fx = GraphFixture::new();
    let a = fx.func("src/keys.rs", "a");
    let b = fx.func("src/keys.rs", "b");
    fx.edge_with(a, b, RelType::Calls, 1.0, "first");
    fx.edge_with(a, b, RelType::Calls, 1.0, "second");
    fx.edge_with(a, b, RelType::Calls, 1.0, "first");
    let bytes = fx.into_bytes();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let result = execute(
        &parse("MATCH ()-[r:Calls]->() RETURN count(DISTINCT r), collect(DISTINCT r)").unwrap(),
        graph,
        None,
        Path::new("."),
    )
    .unwrap();
    assert_eq!(result.rows[0][0], Value::Int(2));
    let Value::List(edges) = &result.rows[0][1] else {
        panic!("expected edge list")
    };
    assert_eq!(edges.len(), 2);
    let result = execute(
        &parse("MATCH ()-[r:Calls]->() WITH r AS edge, count(*) AS n RETURN edge.reason, n")
            .unwrap(),
        graph,
        None,
        Path::new("."),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Str("first".into()), Value::Int(2)],
            vec![Value::Str("second".into()), Value::Int(1)]
        ]
    );
}

#[test]
fn test_execute_single_null_aggregate_preserves_legacy_row_count() {
    let mut fx = GraphFixture::new();
    fx.func("src/null.rs", "one");
    fx.func("src/null.rs", "two");
    let bytes = fx.into_bytes();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    for name in ["count", "sum", "avg", "min", "max", "collect"] {
        for arg in ["*", "null"] {
            let text = format!("MATCH (a:Function) RETURN {name}({arg})");
            let result = execute(&parse(&text).unwrap(), graph, None, Path::new(".")).unwrap();
            assert_eq!(result.rows, vec![vec![Value::Int(2)]], "{text}");
        }
    }
}
const STREAMING_ORACLE_QUERIES: &[&str] = &[
    "MATCH ()-[r]->() RETURN count(r)",
    "MATCH ()-[r]->() WHERE type(r) = 'References' RETURN count(r)",
    "MATCH (a)-[r]->(b) RETURN type(r), count(*)",
    "MATCH (a:Function) OPTIONAL MATCH (a)-[r:Calls]->(b) RETURN a.name, count(r), count(*)",
    "MATCH (a)-[r]->(b) RETURN count(DISTINCT b), collect(DISTINCT b.name)",
    "MATCH (a)-[r]->(b) WITH type(r) AS t, count(*) AS n WHERE n > 1 RETURN t, n",
    "MATCH (a:Function {name:'missing'}) WITH count(*) AS n RETURN n",
    "MATCH (a)-[r]->(b) WITH b AS x RETURN count(DISTINCT x)",
    "MATCH (a:Function) MATCH (a)-[r]->(b) RETURN a.name, count(r)",
    "MATCH (a)-[r]->(b) RETURN type(r) AS t, count(*) AS n ORDER BY n DESC SKIP 1 LIMIT 1",
    "MATCH (a:Function {name:'missing'}) RETURN count(*), sum(a.startLine), collect(a.name)",
    "MATCH (a)-[:Calls]->(b)-[:Calls]->(c) RETURN collect(a.name), count(*)",
    "MATCH (a)-[:Calls*0..2]->(b) RETURN collect(DISTINCT b.name), count(DISTINCT b)",
    "MATCH (a:Function {name:'b'}) OPTIONAL MATCH (c)-[:Calls]->(a) RETURN count(c)",
    "MATCH (a:Function), (b:Function) RETURN count(a), count(b)",
    "MATCH (a)-[r]->(b) RETURN b, count(*)",
];

// Extra shapes that have caused streaming/materialising drift in prior reviews.
// The fixture below gives every successful shape at least one matching row.
const EXTRA_SUCCESS_QUERIES: &[&str] = &[
    "MATCH (a)-[r]->(b) RETURN type(r) AS value, count(*) AS value",
    "MATCH (a)-[r]->(b) WITH type(r) AS value, count(*) AS value RETURN value, count(*)",
    "MATCH (a)-[r]->(b) WITH count(*) AS n RETURN count(n) AS n",
    "MATCH (a)-[r]->(b) WITH b AS x WHERE x.name = 'b' RETURN count(x)",
    "MATCH (a)-[:Calls*1..2]->(b) RETURN count(DISTINCT b)",
    "MATCH (a:Function) MATCH (a)-[r:Calls]->(b) RETURN a.name, count(r)",
    "MATCH (a:Function) MATCH (a)-[r:Calls]->(b), (c:Function) RETURN count(r)",
    "MATCH (a)-[r:Calls]-(b) RETURN count(r)",
    "MATCH (a)-[r:Calls|References]->(b) RETURN count(r)",
    "MATCH (x)-[:Calls]->(y)-[:Calls]->(x) RETURN count(*)",
    "MATCH (a:Function) MATCH (a)-[r:Calls]->(b) WITH a.name AS name, count(r) AS n RETURN name, count(n)",
];

const EXTRA_ERROR_QUERIES: &[&str] = &[
    "MATCH (a)-[r]->(b) WHERE a.uid = 'bad' RETURN type(r), count(*)",
    "MATCH (a)-[r]->(b) WITH type(r) AS t, count(*) AS n WHERE t.uid = 'bad' RETURN t, n",
    "MATCH (a)-[r]->(b) WHERE a.uid = 'bad' WITH a AS x WHERE EXISTS { (x)-[:Calls*0..1]->(y) } RETURN count(*)",
    "MATCH (a)-[r]->(b) WHERE EXISTS { (a)-[:Calls*0..1]->(y) } WITH a AS x WHERE x.uid = 'bad' RETURN count(*)",
];

fn aggregate_oracle_graph() -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    let a = fx.func("src/clean.rs", "a");
    let b = fx.func("src/dirty.rs", "b");
    let c = fx.func("src/dirty.rs", "c");
    fx.edge(a, b, RelType::Calls);
    fx.edge(a, c, RelType::References);
    fx.edge(b, c, RelType::Calls);
    fx.edge(a, b, RelType::Calls);
    fx.edge(c, a, RelType::Calls);
    fx.edge(b, a, RelType::Calls);
    fx.build()
}

fn assert_public_success_matches_materialising(
    text: &str,
    graph: &ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
) {
    let query = parse(text).unwrap_or_else(|err| panic!("parse {text:?}: {err:?}"));
    let fast = execute(&query, graph, view, Path::new("."))
        .unwrap_or_else(|err| panic!("streaming execution {text:?}: {err:?}"));
    let rewritten = pushdown_where(&query);
    let mut cache = ContentCache::new(PathBuf::from("."));
    let slow = execute_materialising(
        rewritten.as_ref().unwrap_or(&query),
        MergedGraph::new(graph, view),
        &mut cache,
    )
    .unwrap_or_else(|err| panic!("materialising execution {text:?}: {err:?}"));
    assert_eq!(fast.columns, slow.columns, "columns: {text}");
    assert_eq!(fast.rows, slow.rows, "rows: {text}");
}

fn assert_public_error_matches_materialising(
    text: &str,
    graph: &ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
) {
    let query = parse(text).unwrap_or_else(|err| panic!("parse {text:?}: {err:?}"));
    let fast = execute(&query, graph, view, Path::new("."))
        .expect_err("streaming path unexpectedly succeeded");
    let rewritten = pushdown_where(&query);
    let mut cache = ContentCache::new(PathBuf::from("."));
    let slow = execute_materialising(
        rewritten.as_ref().unwrap_or(&query),
        MergedGraph::new(graph, view),
        &mut cache,
    )
    .expect_err("materialising path unexpectedly succeeded");
    assert_eq!(format!("{fast:?}"), format!("{slow:?}"), "{text}");
}

#[test]
fn test_execute_aggregate_streaming_matches_materialising_rows() {
    let graph = aggregate_oracle_graph();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    for text in STREAMING_ORACLE_QUERIES {
        assert_public_success_matches_materialising(text, archived, None);
    }
    for text in EXTRA_SUCCESS_QUERIES {
        assert_public_success_matches_materialising(text, archived, None);
    }
    for text in EXTRA_ERROR_QUERIES {
        assert_public_error_matches_materialising(text, archived, None);
    }
    assert_public_error_matches_materialising(
        "MATCH (a:Function) WHERE EXISTS { (x)-[:Calls*0..1]->(y) } RETURN missing, count(*)",
        archived,
        None,
    );
}

#[test]
fn test_execute_aggregate_streaming_matches_materialising_overlay_optional() {
    let graph = aggregate_oracle_graph();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let view = OverlayView::build(
        archived,
        &[OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: "src/dirty.rs".into(),
            symbols: vec![
                OverlaySymbol {
                    is_exported: false,
                    name: "b".into(),
                    kind: NodeKind::Function,
                    owner_class: None,
                    start_line: 1,
                    end_line: 2,
                    start_column: 0,
                    end_column: 0,
                    calls: vec!["a".into()],
                    heritage: Vec::new(),
                    variant_refs: Vec::new(),
                    return_type: None,
                    generic_arity: 0,
                    flags: None,
                },
                OverlaySymbol {
                    is_exported: false,
                    name: "virtual".into(),
                    kind: NodeKind::Function,
                    owner_class: None,
                    start_line: 3,
                    end_line: 4,
                    start_column: 0,
                    end_column: 0,
                    calls: vec!["a".into()],
                    heritage: Vec::new(),
                    variant_refs: Vec::new(),
                    return_type: None,
                    generic_arity: 0,
                    flags: None,
                },
            ],
            imports: vec![],
        }],
    )
    .unwrap();
    assert_public_success_matches_materialising(
        "MATCH (a:Function) OPTIONAL MATCH (a)-[r:Calls]->(b) WITH a.name AS name, count(r) AS n RETURN name, count(n)",
        archived,
        Some(&view),
    );
    for text in [
        "MATCH (a:Function) MATCH (a)-[r:Calls]->(b) RETURN a.name, count(r)",
        "MATCH (a:Function) WITH a AS x WHERE x.name <> 'missing' RETURN x.name, count(*)",
        "MATCH (a:Function) OPTIONAL MATCH (a)-[r:Calls]->(b) RETURN a.name, count(r), count(*)",
    ] {
        assert_public_success_matches_materialising(text, archived, Some(&view));
    }
}

#[test]
fn test_execute_aggregate_streaming_matches_materialising_who_calls_rows() {
    let graph = who_calls_graph();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    for text in WHO_CALLS_QUERIES {
        assert_public_success_matches_materialising(text, archived, None);
    }
    for text in WHO_CALLS_QUERIES {
        let aggregate_text = match text.rsplit_once(" LIMIT ") {
            Some((before_limit, limit)) => format!("{before_limit}, count(*) LIMIT {limit}"),
            None => format!("{text}, count(*)"),
        };
        assert_public_success_matches_materialising(&aggregate_text, archived, None);
    }
}

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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    f(archived);
}

// -----------------------------------------------------------------------
// Single-hop MATCH
// -----------------------------------------------------------------------

#[test]
fn exec_single_hop_returns_one_row() {
    with_two(|g| {
        let q = parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, b.name").unwrap();
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
            "MATCH (a:Function)-[:Calls*1..3]->(b:Function) WHERE a.name = 'a' RETURN DISTINCT b.name",
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
fn test_parse_var_len_minimum_above_one_is_refused() {
    // A walk-based expansion cannot enforce openCypher's no-repeated-relationship
    // rule, so `*2..3` is refused rather than answered from walks.
    let error = parse(
        "MATCH (a:Function)-[:Calls*2..3]->(b:Function) WHERE a.name = 'a' RETURN DISTINCT b.name",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("minimum above 1"), "{error}");
}

// -----------------------------------------------------------------------
// Bidirectional and reverse arrows
// -----------------------------------------------------------------------

#[test]
fn exec_reverse_arrow() {
    // callee <-[:Calls]- caller  →  same edge, traversed in reverse
    with_two(|g| {
        let q = parse("MATCH (b:Function)<-[:Calls]-(a:Function) RETURN a.name, b.name").unwrap();
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
        let q = parse("MATCH (a:Function)-[:Calls]-(b:Function) RETURN a.name, b.name").unwrap();
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

fn first_cell(g: &crate::graph::ArchivedZeroCopyGraph, query: &str) -> Value {
    let q = parse(query).unwrap();
    let r = execute(&q, g, None, Path::new(".")).unwrap();
    r.rows[0][0].clone()
}

#[test]
fn test_list_index_labels_first_returns_label_string() {
    with_two(|g| {
        let v = first_cell(
            g,
            "MATCH (a:Function) WHERE a.name = 'caller' RETURN labels(a)[0]",
        );
        assert_eq!(v, Value::Str("Function".into()));
    });
}

#[test]
fn test_list_index_negative_counts_from_end() {
    with_two(|g| {
        let v = first_cell(
            g,
            "MATCH (a:Function) WHERE a.name = 'caller' RETURN labels(a)[-1]",
        );
        assert_eq!(v, Value::Str("Function".into()));
    });
}

#[test]
fn test_list_index_out_of_range_returns_null() {
    with_two(|g| {
        for idx in ["5", "-5"] {
            let q = format!("MATCH (a:Function) WHERE a.name = 'caller' RETURN labels(a)[{idx}]");
            assert_eq!(first_cell(g, &q), Value::Null, "index {idx}");
        }
    });
}

#[test]
fn test_list_index_on_null_returns_null() {
    with_two(|g| {
        // `labels(x)` of an unbound variable is Null.
        let v = first_cell(g, "MATCH (a:Function) RETURN labels(zz)[0]");
        assert_eq!(v, Value::Null);
    });
}

#[test]
fn test_list_index_on_non_list_returns_null() {
    with_two(|g| {
        let v = first_cell(
            g,
            "MATCH (a:Function) WHERE a.name = 'caller' RETURN a.name[0]",
        );
        assert_eq!(v, Value::Null);
        let v = first_cell(
            g,
            "MATCH (a:Function) WHERE a.name = 'caller' RETURN id(a)[0]",
        );
        assert_eq!(v, Value::Null);
    });
}

#[test]
fn test_list_index_on_property_list_chains() {
    with_two(|g| {
        // `a.decorators` is a list property (empty here) -> out of range Null.
        let v = first_cell(
            g,
            "MATCH (a:Function) WHERE a.name = 'caller' RETURN a.decorators[0]",
        );
        assert_eq!(v, Value::Null);
    });
}

#[test]
fn test_list_index_literal_list_and_chain() {
    with_two(|g| {
        let v = first_cell(g, "MATCH (a:Function) RETURN [10,20,30][1]");
        assert_eq!(v, Value::Int(20));
        let v = first_cell(g, "MATCH (a:Function) RETURN [[1,2],[3,4]][1][0]");
        assert_eq!(v, Value::Int(3));
    });
}

#[test]
fn test_list_slice_bounds_return_sublist() {
    with_two(|g| {
        let ints = |xs: &[i64]| Value::List(xs.iter().map(|i| Value::Int(*i)).collect());
        let cases: [(&str, &[i64]); 6] = [
            ("[1,2,3,4][1..3]", &[2, 3]),
            ("[1,2,3,4][..2]", &[1, 2]),
            ("[1,2,3,4][2..]", &[3, 4]),
            ("[1,2,3,4][-2..]", &[3, 4]),
            ("[1,2,3,4][1..99]", &[2, 3, 4]),
            ("[1,2,3,4][3..1]", &[]),
        ];
        for (expr, want) in cases {
            let v = first_cell(g, &format!("MATCH (a:Function) RETURN {expr}"));
            assert_eq!(v, ints(want), "{expr}");
        }
    });
}

#[test]
fn test_list_index_in_where_filters_rows() {
    with_two(|g| {
        let q = parse("MATCH (a:Function) WHERE labels(a)[0] = 'Function' RETURN a.name").unwrap();
        let r = execute(&q, g, None, Path::new(".")).unwrap();
        assert_eq!(r.rows.len(), 2);
        let q = parse("MATCH (a:Function) WHERE labels(a)[0] = 'Class' RETURN a.name").unwrap();
        let r = execute(&q, g, None, Path::new(".")).unwrap();
        assert_eq!(r.rows.len(), 0);
    });
}

#[test]
fn test_list_index_aggregate_subscript_returns_parse_error() {
    for q in [
        "MATCH (a:Function) RETURN collect(a.name)[0]",
        "MATCH (a:Function) RETURN count(*)[..1]",
        "MATCH (a:Function) WITH collect(a.name)[0] AS x RETURN x",
    ] {
        let err = parse(q).expect_err(q).to_string();
        assert!(err.contains("WITH collect(x) AS xs"), "{q}: {err}");
    }
}

#[test]
fn test_list_index_named_aggregate_returns_first_element() {
    with_two(|g| {
        let v = first_cell(
            g,
            "MATCH (a:Function) WHERE a.name = 'caller' WITH collect(a.name) AS xs RETURN xs[0]",
        );
        assert_eq!(v, Value::Str("caller".into()));
    });
}

#[test]
fn test_list_index_string_key_on_node_returns_property() {
    with_two(|g| {
        let v = first_cell(
            g,
            "MATCH (a:Function) WHERE a['name'] = 'caller' RETURN a['name']",
        );
        assert_eq!(v, Value::Str("caller".into()));
    });
}

#[test]
fn test_list_index_two_subscripts_of_one_base_return_distinct_columns() {
    with_two(|g| {
        let q = parse(
            "MATCH (a:Function) WHERE a.name = 'caller' RETURN labels(a)[0], labels(a)[-1], labels(a)[..1]",
        )
        .unwrap();
        let r = execute(&q, g, None, Path::new(".")).unwrap();
        assert_eq!(
            r.columns,
            ["LABELS(*)[0]", "LABELS(*)[-1]", "LABELS(*)[..1]"]
        );
    });
}

#[test]
fn exec_scalar_mixed_with_aggregate() {
    // type(r) used as group key alongside count(*) aggregate.
    with_two(|g| {
        let q = parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN type(r), count(*) AS c")
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
        let q =
            parse("MATCH (a:Function) WHERE 'Function' IN labels(a) RETURN a.name ORDER BY a.name")
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

        let return_q = parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN TYPE(r)").unwrap();
        let return_r = execute(&return_q, g, None, Path::new(".")).unwrap();

        let with_q =
            parse("MATCH (a:Function)-[r:Calls]->(b:Function) WITH TYPE(r) AS t RETURN t").unwrap();
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
        let q =
            parse("MATCH (a:Function) WITH a AS x WHERE ID(x) IS NOT NULL RETURN x.name").unwrap();
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
    // the new binding (callee, idx 1), but `rebind_one` deliberately
    // preserves the OLD node_vars["a"] (caller, idx 0) unchanged for downstream MATCH traversal. `ID(a) = 1` only
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
        acc.feed(Value::Int(1), false, None);
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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
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
        let q = parse("MATCH (a:Function) OPTIONAL MATCH (a)-[:Calls]->(b) RETURN a.name, b.name")
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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
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
        let q = parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN a.name, COUNT(*)").unwrap();
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
        let q = parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN COUNT(DISTINCT b.name)")
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
        let q =
            parse("MATCH (a:Function)-[r:Calls]->(b:Function) RETURN COUNT(DISTINCT r)").unwrap();
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
        let q = parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN COLLECT(b.name)").unwrap();
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
        let q =
            parse("MATCH (a:Function)-[:Calls]->(b:Function) WITH a.name AS nm RETURN nm").unwrap();
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
        let q = parse("MATCH (a:Function) OPTIONAL MATCH (a)-[:Calls]->(b) RETURN a.name, b.name")
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
        "MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) MATCH (g:Function) WHERE c.name = 'fan' RETURN f.name",
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
            "MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) WITH f, c WHERE c IS NULL RETURN f.name",
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
            "MATCH (f:Function) OPTIONAL MATCH (c)-[:Calls]->(f) WITH f, c WHERE c IS NOT NULL RETURN f.name ORDER BY f.name",
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
        let q = parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN DISTINCT a.name").unwrap();
        let r = execute(&q, g, None, Path::new(".")).unwrap();
        assert_eq!(r.rows.len(), 1, "expected 1 distinct row, got {:?}", r.rows);
        assert_eq!(r.rows[0][0], Value::Str("fan".into()));
    });
}

#[test]
fn exec_skip_and_limit() {
    // 3 nodes sorted by name asc: fan, leaf_a, leaf_b. SKIP 1 LIMIT 1 → leaf_a.
    with_fan(|g| {
        let q =
            parse("MATCH (a:Function) RETURN a.name ORDER BY a.name ASC SKIP 1 LIMIT 1").unwrap();
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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
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
    // UNION concatenates results from two sub-queries that name their columns alike.
    with_func_and_method(|g| {
        let q = parse(
            "MATCH (a:Function) RETURN a.name AS name UNION MATCH (b:Method) RETURN b.name AS name",
        )
        .unwrap();
        let r = execute(&q, g, None, Path::new(".")).unwrap();
        // 1 Function + 1 Method = 2 rows (distinct by default).
        assert_eq!(r.columns, vec!["name"]);
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
fn exec_union_different_column_names_is_refused() {
    // Labelling the right branch's rows with the left branch's name hid that
    // the two columns mean different things. Old: succeeded, columns ["a.name"].
    with_func_and_method(|g| {
        let q =
            parse("MATCH (a:Function) RETURN a.name UNION MATCH (b:Method) RETURN b.name").unwrap();
        let error = execute(&q, g, None, Path::new("."))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("[a.name]") && error.contains("[b.name]"),
            "{error}"
        );
    });
}

#[test]
fn exec_union_all_keeps_dupes() {
    // UNION ALL keeps duplicates; matching all :Function nodes gives 1 from left, 1 from right.
    // Actually in this fixture Function=my_func, Method=my_method.
    // Use fan fixture where there are 3 Function nodes.
    with_fan(|g| {
        // Both sides match all Function nodes → 6 rows with UNION ALL.
        let q =
            parse("MATCH (a:Function) RETURN a.name UNION ALL MATCH (a:Function) RETURN a.name")
                .unwrap();
        let r = execute(&q, g, None, Path::new(".")).unwrap();
        assert_eq!(r.rows.len(), 6, "UNION ALL keeps duplicates: 3+3");
    });
}

#[test]
fn exec_union_dedupes_without_all() {
    // UNION (no ALL) deduplicates.
    with_fan(|g| {
        let q = parse("MATCH (a:Function) RETURN a.name UNION MATCH (a:Function) RETURN a.name")
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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();

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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    f(archived);
}

// a) async-only filter returns async functions, excludes sync
#[test]
fn fm_is_async_filter_returns_async_excludes_sync() {
    with_fm(|g| {
        let q = parse("MATCH (f:Function|Method) WHERE f.is_async = true RETURN f.name").unwrap();
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
        let q = parse("MATCH (f:Function|Method) WHERE f.is_test = true RETURN f.name").unwrap();
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
        let q = parse("MATCH (m:Function|Method) WHERE 'Override' IN m.decorators RETURN m.name")
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
        let q_py = parse("MATCH (f:Function|Method) WHERE 'app.get' IN f.decorators RETURN f.name")
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
        let q = parse("MATCH (f:Function|Method) WHERE f.visibility = 2 RETURN f.name").unwrap();
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
        let q =
            parse("MATCH (f:Function) WHERE f.name = 'sync_fn' RETURN f.is_async, f.decorators")
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
        let q = parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN a.name, b.name").unwrap();
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
        let q = parse("MATCH (m:Function|Method) WHERE 'Override' IN m.decorators RETURN m.name")
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
        let q =
            parse("MATCH (a:Function) WHERE a.name IN ['caller', 'callee'] RETURN a.name").unwrap();
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
        let q = parse("MATCH (a:Function)-[:Calls]->(b:Function) RETURN COLLECT(b.name)").unwrap();
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
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
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
fn test_parse_exists_var_len_minimum_above_one_is_refused() {
    for range in ["*2..3", "*3..4", "*2..2"] {
        let error = parse(&format!(
            "MATCH (f:Function {{name: 'c'}}) WHERE EXISTS((x)-[:Calls{range}]->(f)) RETURN f.name"
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("minimum above 1"), "{range}: {error}");
    }
}

#[test]
fn eval_exists_var_len_range_respects_upper_bound() {
    // chain a→b→c: c is two hops from a, so *1..1 does not see a but *1..2 does.
    with_three(|g| {
        let run = |range: &str| {
            let q = parse(&format!(
                "MATCH (f:Function {{name: 'c'}}) WHERE EXISTS((a {{name: 'a'}})-[:Calls{range}]->(f)) RETURN f.name"
            ))
            .unwrap();
            execute(&q, g, None, Path::new(".")).unwrap().rows.len()
        };
        assert_eq!(run("*1..2"), 1, "a→b→c is a depth-2 path into c");
        assert_eq!(run("*0..1"), 0, "no depth-1 edge a→c");
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
        let q =
            parse("MATCH (f:Function {name: 'a'}) WHERE EXISTS((x)-[:Calls]->(y)) RETURN f.name")
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
        let q =
            parse("MATCH (f:Function) WHERE EXISTS((x)-[:Calls]->(y)-[:Calls]->(z)) RETURN f.name")
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

// -----------------------------------------------------------------------
// Who-calls fast path (`far_end_name_seeds`)
// -----------------------------------------------------------------------

/// nodes: 0 zeta(F a.ts) · 1 parse(F a.ts) · 2 alpha(M b.ts) · 3 parse(M b.ts)
///        4 beta(F c.ts) · 5 lonely(F c.ts) · 6 ""(F d.ts, tombstone) · 7 gamma(F d.ts)
/// Edges are inserted out of source order, so the forward row order
/// (caller idx, then out-edge order) differs from both insertion order
/// and the reversed spelling's order (callee idx, then in-edge order).
fn who_calls_graph() -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    let zeta = fx.func("src/a.ts", "zeta");
    let parse_a = fx.func("src/a.ts", "parse");
    let alpha = fx.method("src/b.ts", "Parser", "alpha");
    let parse_b = fx.method("src/b.ts", "Parser", "parse");
    let beta = fx.func("src/c.ts", "beta");
    let lonely = fx.func("src/c.ts", "lonely");
    let tomb = fx.func("src/d.ts", "");
    let gamma = fx.func("src/d.ts", "gamma");
    fx.edge_with(beta, parse_a, RelType::Calls, 1.0, "beta-p1");
    fx.edge_with(zeta, parse_b, RelType::Calls, 1.0, "zeta-p3-first");
    fx.edge_with(zeta, parse_b, RelType::Calls, 1.0, "zeta-p3-second");
    fx.edge_with(alpha, parse_a, RelType::Calls, 1.0, "alpha-p1");
    fx.edge_with(alpha, parse_b, RelType::Calls, 1.0, "alpha-p3");
    fx.edge_with(zeta, parse_a, RelType::Calls, 1.0, "zeta-p1");
    fx.edge_with(alpha, lonely, RelType::Calls, 1.0, "alpha-lonely");
    fx.edge_with(beta, parse_b, RelType::ReadsField, 1.0, "beta-reads-p3");
    fx.edge_with(gamma, tomb, RelType::Calls, 1.0, "gamma-tomb");
    fx.build()
}

fn rows_of(g: &ZeroCopyGraph, query: &str) -> Vec<Vec<Value>> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(g).unwrap();
    let archived =
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let q = parse(query).unwrap();
    execute(&q, archived, None, Path::new(".")).unwrap().rows
}

/// Eligibility of the first pattern of `query`, after the same WHERE
/// pushdown `execute` applies. `use_kind_csr` is false: these probes
/// carry no first-node label, so the scan they replace is the plain one.
fn fast_path_seeds(g: &ZeroCopyGraph, query: &str) -> Option<Vec<u32>> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(g).unwrap();
    let archived =
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let q = parse(query).unwrap();
    let q = pushdown_where(&q).unwrap_or(q);
    far_end_name_seeds(
        &q.matches[0].patterns[0],
        &Binding::default(),
        MergedGraph::new(archived, None),
        false,
    )
}

fn row_strings(rows: &[Vec<Value>]) -> Vec<String> {
    let mut out: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

const WHO_CALLS_QUERIES: &[&str] = &[
    "MATCH (a)-[r:Calls]->(b {name:'parse'}) RETURN a.name, b.filePath, r.reason",
    "MATCH (a)-[r:Calls]->(b {name:'parse'}) RETURN a.name, r.reason LIMIT 2",
    "MATCH (a)-[:Calls]->(b {name:'nope'}) RETURN a.name",
    "MATCH (a)-[r:Calls]->(b) WHERE b.name = 'parse' RETURN a.name, r.reason",
    "MATCH (a {name:'zeta'})-[r:Calls]->(b {name:'parse'}) RETURN a.name, r.reason",
    "MATCH (a:Callable)-[r:Calls]->(b {name:'parse'}) RETURN a.name, r.reason",
    "MATCH (a:Function)-[r:Calls]->(b {name:'parse'}) RETURN a.name, r.reason",
    "MATCH (a:Method|Function)-[r:Calls]->(b {name:'parse'}) RETURN a.name, r.reason",
    "MATCH (a:Function|Callable)-[r:Calls]->(b {name:'parse'}) RETURN a.name, r.reason",
    "MATCH (a)<-[r:Calls]-(b {name:'zeta'}) RETURN a.name, a.filePath, r.reason",
    "MATCH (a)-[r]->(b {name:'parse'}) RETURN a.name, r.reason",
    "MATCH (a)-[r:Imports]->(b {name:'parse'}) RETURN a.name",
    "MATCH (a)-[r:Calls]->(b {name:''}) RETURN a.name, r.reason",
    "MATCH ()-[r:Calls]->({name:'parse'}) RETURN r.reason",
    "MATCH (a)-[r:Calls*1..1]->(b {name:'parse'}) RETURN a.name",
    "MATCH (x {name:'lonely'}) MATCH (a)-[r:Calls]->(b {name:'parse'}) RETURN x.name, a.name, r.reason",
    "MATCH (x {name:'lonely'}) OPTIONAL MATCH (a)-[r:Calls]->(b {name:'parse'}) RETURN x.name, a.name, r.reason",
    "MATCH (a)-[:Calls]->(b {name:'parse'}) RETURN COUNT(a)",
    "MATCH (a {name:'zeta'}) MATCH (a)-[r:Calls]->(b {name:'parse'}) RETURN a.name, r.reason",
];

/// Contract: the fast path changes time and memory only. Every query's
/// rows, in order, equal the full-scan rows; clearing `name_index`
/// forces the full scan (the guard in `far_end_name_seeds`).
#[test]
fn test_far_end_name_seeds_who_calls_queries_match_full_scan_rows() {
    let fast = who_calls_graph();
    let mut slow = who_calls_graph();
    slow.name_index.clear();
    assert!(!fast.name_index.is_empty());
    for query in WHO_CALLS_QUERIES {
        assert_eq!(rows_of(&fast, query), rows_of(&slow, query), "{query}");
    }
}

/// Same contract on a graph without the kind CSR: a labelled first node
/// then scans in node order instead of kind-grouped order.
#[test]
fn test_far_end_name_seeds_without_kind_csr_match_full_scan_rows() {
    let strip_csr = |mut g: ZeroCopyGraph| {
        g.kind_offsets.clear();
        g.kind_node_idx.clear();
        g
    };
    let fast = strip_csr(who_calls_graph());
    let mut slow = strip_csr(who_calls_graph());
    slow.name_index.clear();
    for query in WHO_CALLS_QUERIES {
        assert_eq!(rows_of(&fast, query), rows_of(&slow, query), "{query}");
    }
}

/// Contract: forward order is caller idx ascending, then that caller's
/// out-edges in edge order; parallel edges each keep their row. The
/// reversed spelling returns the same multiset in callee order.
#[test]
fn test_far_end_name_seeds_forward_order_keeps_parallel_edges() {
    let g = who_calls_graph();
    let fwd = rows_of(
        &g,
        "MATCH (a)-[r:Calls]->(b {name:'parse'}) RETURN a.name, b.filePath, r.reason",
    );
    let row = |a: &str, f: &str, r: &str| {
        vec![
            Value::Str(a.into()),
            Value::Str(f.into()),
            Value::Str(r.into()),
        ]
    };
    assert_eq!(
        fwd,
        vec![
            row("zeta", "src/b.ts", "zeta-p3-first"),
            row("zeta", "src/b.ts", "zeta-p3-second"),
            row("zeta", "src/a.ts", "zeta-p1"),
            row("alpha", "src/a.ts", "alpha-p1"),
            row("alpha", "src/b.ts", "alpha-p3"),
            row("beta", "src/a.ts", "beta-p1"),
        ]
    );
    let rev = rows_of(
        &g,
        "MATCH (b {name:'parse'})<-[r:Calls]-(a) RETURN a.name, b.filePath, r.reason",
    );
    assert_ne!(
        fwd, rev,
        "fixture must make the two spellings' orders differ"
    );
    assert_eq!(row_strings(&fwd), row_strings(&rev));
}

#[test]
fn test_far_end_name_seeds_category_label_keeps_kind_grouped_order() {
    let g = who_calls_graph();
    let names: Vec<Value> = rows_of(
        &g,
        "MATCH (a:Callable)-[:Calls]->(b {name:'parse'}) RETURN a.name",
    )
    .into_iter()
    .map(|mut r| r.remove(0))
    .collect();
    let s = |n: &str| Value::Str(n.into());
    // Function callers (zeta, beta) before the Method caller (alpha).
    assert_eq!(
        names,
        vec![
            s("zeta"),
            s("zeta"),
            s("zeta"),
            s("beta"),
            s("alpha"),
            s("alpha")
        ]
    );
}

#[test]
fn test_far_end_name_seeds_who_calls_shape_returns_callers() {
    let g = who_calls_graph();
    assert_eq!(
        fast_path_seeds(&g, "MATCH (a)-[:Calls]->(b {name:'parse'}) RETURN a.name"),
        Some(vec![0, 2, 4])
    );
    assert_eq!(
        fast_path_seeds(
            &g,
            "MATCH (a)-[:Calls]->(b) WHERE b.name = 'parse' RETURN a.name"
        ),
        Some(vec![0, 2, 4])
    );
    assert_eq!(
        fast_path_seeds(&g, "MATCH (a)<-[:Calls]-(b {name:'zeta'}) RETURN a.name"),
        Some(vec![1, 3])
    );
    assert_eq!(
        fast_path_seeds(&g, "MATCH (a)-[:Calls]->(b {name:'nope'}) RETURN a.name"),
        Some(vec![])
    );
    assert_eq!(
        fast_path_seeds(&g, "MATCH (a)-[:Imports]->(b {name:'parse'}) RETURN a.name"),
        Some(vec![])
    );
}

#[test]
fn test_far_end_name_seeds_ineligible_shapes_return_none() {
    let g = who_calls_graph();
    for query in [
        "MATCH (a {name:'zeta'})-[:Calls]->(b {name:'parse'}) RETURN a.name",
        "MATCH (a)-[:Calls]-(b {name:'parse'}) RETURN a.name",
        "MATCH (a)-[:Calls*1..2]->(b {name:'parse'}) RETURN DISTINCT a.name",
        "MATCH (a)-[:Calls]->(b {name:''}) RETURN a.name",
        "MATCH (a)-[:Calls]->(b {name:1}) RETURN a.name",
        "MATCH (a)-[:Calls]->(b:Function) RETURN a.name",
        "MATCH (a)-[:Calls]->(b)-[:Calls]->(c {name:'parse'}) RETURN a.name",
    ] {
        assert_eq!(fast_path_seeds(&g, query), None, "{query}");
    }
    let mut no_index = who_calls_graph();
    no_index.name_index.clear();
    assert_eq!(
        fast_path_seeds(
            &no_index,
            "MATCH (a)-[:Calls]->(b {name:'parse'}) RETURN a.name"
        ),
        None
    );
}

/// The name index skips tombstones (empty name); the scan still matches
/// them, so an empty-name query must keep the scan and find the caller.
#[test]
fn test_far_end_name_seeds_empty_name_keeps_tombstone_rows() {
    let g = who_calls_graph();
    assert_eq!(
        rows_of(
            &g,
            "MATCH (a)-[r:Calls]->(b {name:''}) RETURN a.name, r.reason"
        ),
        vec![vec![
            Value::Str("gamma".into()),
            Value::Str("gamma-tomb".into())
        ]]
    );
}

#[test]
fn test_far_end_name_seeds_empty_graph_returns_no_rows() {
    let g = GraphFixture::new().build();
    assert!(rows_of(&g, "MATCH (a)-[:Calls]->(b {name:'parse'}) RETURN a.name").is_empty());
}

/// With an overlay the fast path stands down; the overlay's own Calls
/// edge from the re-parsed `keep_fn` still answers who calls `target_fn`.
#[test]
fn test_far_end_name_seeds_with_overlay_keeps_full_scan() {
    use crate::session::{OverlayFileInput, OverlaySymbol};
    let mut fx = GraphFixture::new();
    let keep = fx.func("src/dirty.rs", "keep_fn");
    let gone = fx.func("src/dirty.rs", "gone_fn");
    let caller = fx.func("src/clean.rs", "clean_caller");
    let target = fx.func("src/clean.rs", "target_fn");
    fx.edge(caller, keep, RelType::Calls);
    fx.edge(keep, target, RelType::Calls);
    fx.edge(caller, gone, RelType::Calls);
    let bytes = fx.into_bytes();
    let archived =
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let dirty = OverlayFileInput {
        export_aliases: Vec::new(),
        rel_path: "src/dirty.rs".to_string(),
        symbols: vec![OverlaySymbol {
            is_exported: false,
            name: "keep_fn".to_string(),
            kind: NodeKind::Function,
            owner_class: None,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            calls: vec!["target_fn".to_string()],
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
        }],
        imports: vec![],
    };
    let view = OverlayView::build(archived, &[dirty]).unwrap();
    let q = parse("MATCH (a)-[:Calls]->(b {name:'target_fn'}) RETURN a.name, b.name").unwrap();
    assert_eq!(
        far_end_name_seeds(
            &q.matches[0].patterns[0],
            &Binding::default(),
            MergedGraph::new(archived, Some(&view)),
            false,
        ),
        None
    );
    let r = execute(&q, archived, Some(&view), Path::new(".")).unwrap();
    assert_eq!(
        r.rows,
        vec![vec![
            Value::Str("keep_fn".into()),
            Value::Str("target_fn".into())
        ]]
    );
}

/// A rebuilt lexical closure reference keeps the full index's reason in
/// every way cypher reads an overlay edge, not the uncommitted-edit marker.
#[test]
fn test_execute_overlay_closure_reference_reports_lexical_reason() {
    use crate::session::{OverlayFileInput, OverlaySymbol};
    let bytes = GraphFixture::new().into_bytes();
    let archived =
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let symbol = |name: &str, start_line, end_line| OverlaySymbol {
        is_exported: false,
        name: name.to_string(),
        kind: NodeKind::Function,
        owner_class: None,
        start_line,
        end_line,
        start_column: 0,
        end_column: 0,
        calls: vec![],
        heritage: Vec::new(),
        variant_refs: Vec::new(),
        return_type: None,
        generic_arity: 0,
        flags: None,
    };
    let dirty = OverlayFileInput {
        export_aliases: Vec::new(),
        rel_path: "src/app.ts".to_string(),
        symbols: vec![symbol("enclosing", 1, 9), symbol("<anonymous:3:2>", 3, 4)],
        imports: vec![],
    };
    let view = OverlayView::build(archived, &[dirty]).unwrap();
    let reason = Value::Str(crate::analyzer::types::CLOSURE_REFERENCE_REASON.into());
    let filtered = format!(
        "MATCH (a)-[r:References]->(b) WHERE r.reason = '{}' RETURN a.name, r.reason",
        crate::analyzer::types::CLOSURE_REFERENCE_REASON
    );
    let r = execute(
        &parse(&filtered).unwrap(),
        archived,
        Some(&view),
        Path::new("."),
    )
    .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::Str("enclosing".into()), reason.clone()]]
    );
    let q = parse("MATCH (a)-[r:References]->(b) RETURN r").unwrap();
    let r = execute(&q, archived, Some(&view), Path::new(".")).unwrap();
    assert_eq!(
        r.rows,
        vec![vec![
            Value::Str("References".into()),
            Value::Float(1.0),
            reason
        ]]
    );
}

// -----------------------------------------------------------------------
// First-node name seeds (`first_node_name`)
// -----------------------------------------------------------------------

/// Many same-name nodes of mixed kinds and files, a tombstone, a unicode
/// name, and one name shared by 1200 nodes (see `same_name_nodes`).
fn same_name_graph() -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    fx.same_name_nodes(
        "shared",
        1200,
        |i| format!("src/shared{i}.ts"),
        |fx, i, path, _| {
            if i % 100 == 0 {
                fx.func(path, "");
                fx.func(path, "naïve_函数");
                fx.func(path, "rare");
            }
        },
    );
    fx.build()
}

const FIRST_NODE_NAME_QUERIES: &[&str] = &[
    "MATCH (n {name:'shared'}) RETURN n.name, n.filePath, n.kind",
    "MATCH ({name:'shared'}) RETURN count(*)",
    "MATCH (n {name:'shared', kind:'Method'}) RETURN n.filePath",
    "MATCH (n:Class {name:'shared'}) RETURN n.filePath",
    "MATCH (n:Function|Method {name:'shared'}) RETURN n.filePath",
    "MATCH (n {name:'rare'}) RETURN n.filePath",
    "MATCH (n {name:'naïve_函数'}) RETURN n.filePath",
    "MATCH (n {name:'absent'}) RETURN n.filePath",
    "MATCH (n {name:''}) RETURN count(*)",
    "MATCH (n {name:1}) RETURN count(*)",
    "MATCH (n) WHERE n.name = 'shared' RETURN n.filePath, n.kind",
];

/// Contract: the name index changes time only. Every query's rows, in
/// order, equal the full-scan rows; clearing `name_index` forces the scan.
#[test]
fn test_first_node_name_queries_match_full_scan_rows() {
    let fast = same_name_graph();
    let mut slow = same_name_graph();
    slow.name_index.clear();
    assert!(!fast.name_index.is_empty());
    for query in FIRST_NODE_NAME_QUERIES {
        assert_eq!(rows_of(&fast, query), rows_of(&slow, query), "{query}");
    }
}

#[test]
fn test_first_node_name_shared_name_returns_every_node_ascending() {
    let g = same_name_graph();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap();
    let archived =
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let q = parse("MATCH (n {name:'shared'}) RETURN n.name").unwrap();
    let mg = MergedGraph::new(archived, None);
    let name = first_node_name(&q.matches[0].patterns[0].nodes[0], mg)
        .expect("plain name literal, no overlay");
    let seeds: Vec<u32> = mg.name_candidates(name).collect();
    assert_eq!(seeds.len(), 1200);
    assert!(seeds.windows(2).all(|w| w[0] < w[1]), "ascending");
    assert_eq!(rows_of(&g, FIRST_NODE_NAME_QUERIES[0]).len(), 1200);
}

#[test]
fn test_first_node_name_ineligible_inputs_scan_or_return_none() {
    let g = same_name_graph();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap();
    let archived =
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let mg = MergedGraph::new(archived, None);
    let name_of = |query: &str| {
        let q = parse(query).unwrap();
        first_node_name(&q.matches[0].patterns[0].nodes[0], mg).map(str::to_owned)
    };
    assert_eq!(name_of("MATCH (n {name:1}) RETURN n"), None);
    assert_eq!(name_of("MATCH (n {kind:'Class'}) RETURN n"), None);
    assert_eq!(name_of("MATCH (n) RETURN n"), None);
    // An empty name stays eligible: `name_candidates` itself falls back to
    // every node, because the index skips the tombstone the scan matches.
    assert_eq!(name_of("MATCH (n {name:''}) RETURN n").as_deref(), Some(""));
    assert_eq!(mg.name_candidates("").count(), archived.nodes.len());
    assert_eq!(mg.name_candidates("absent").count(), 0);

    let mut no_index = same_name_graph();
    no_index.name_index.clear();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&no_index).unwrap();
    let archived =
        rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let q = parse("MATCH (n {name:'shared'}) RETURN n").unwrap();
    let mg = MergedGraph::new(archived, None);
    let name = first_node_name(&q.matches[0].patterns[0].nodes[0], mg).unwrap();
    assert_eq!(mg.name_candidates(name).count(), archived.nodes.len());
}

/// The tombstone (empty name) is matched by the scan only, so an
/// empty-name query must keep the scan and still return it.
#[test]
fn test_first_node_name_empty_name_keeps_tombstone_rows() {
    let g = same_name_graph();
    assert_eq!(
        rows_of(&g, "MATCH (n {name:''}) RETURN count(*)"),
        vec![vec![Value::Int(12)]]
    );
}

/// With an overlay the seeds stand down: rows equal the scan's with the
/// index cleared, and a base node the view suppresses stays hidden.
#[test]
fn test_first_node_name_with_overlay_matches_scan_and_hides_suppressed() {
    use crate::session::{OverlayFileInput, OverlaySymbol};
    let graph = || {
        let mut fx = GraphFixture::new();
        fx.func("src/dirty.rs", "keep_fn");
        fx.func("src/dirty.rs", "gone_fn");
        fx.func("src/clean.rs", "keep_fn");
        fx.build()
    };
    let dirty = || OverlayFileInput {
        export_aliases: Vec::new(),
        rel_path: "src/dirty.rs".to_string(),
        symbols: vec![OverlaySymbol {
            is_exported: false,
            name: "keep_fn".to_string(),
            kind: NodeKind::Function,
            owner_class: None,
            start_line: 5,
            end_line: 6,
            start_column: 0,
            end_column: 0,
            calls: vec![],
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
        }],
        imports: vec![],
    };
    let rows_with_view = |g: ZeroCopyGraph, query: &str| {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        let view = OverlayView::build(archived, &[dirty()]).unwrap();
        let q = parse(query).unwrap();
        assert_eq!(
            first_node_name(
                &q.matches[0].patterns[0].nodes[0],
                MergedGraph::new(archived, Some(&view))
            ),
            None
        );
        execute(&q, archived, Some(&view), Path::new("."))
            .unwrap()
            .rows
    };
    let mut no_index = graph();
    no_index.name_index.clear();
    let keep = "MATCH (n {name:'keep_fn'}) RETURN n.filePath, n.startLine";
    assert_eq!(
        rows_with_view(graph(), keep),
        rows_with_view(no_index, keep)
    );
    assert!(rows_with_view(graph(), "MATCH (n {name:'gone_fn'}) RETURN n.filePath").is_empty());
}
