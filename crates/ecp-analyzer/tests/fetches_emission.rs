//! Fetches-edge emission integration tests — spec §7 / PR §8.5.
//!
//! Verifies that Pass 1.6b correctly emits `RelType::Fetches` edges from
//! HTTP-client call sites to matching `Route` nodes within the same graph.
//! Each fixture writes real files to disk so the builder can scan content,
//! but Route data comes from `LocalGraph.routes` (parser output), not disk.
//!
//! Coverage:
//! 1. TS exact match → 0.8 confidence
//! 2. Python exact match → 0.8 confidence
//! 3. TS templated match → 0.6 confidence
//! 4. Method mismatch → 0 edges
//! 5. Cross-repo miss (external host) → 0 edges (silent skip)
//! 6. Query-string stripping → 1 edge (matched)

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{NodeKind, RelType};

// ─── LocalGraph helpers ──────────────────────────────────────────────────────

fn raw_node(name: &str) -> RawNode {
    RawNode {
        name: name.into(),
        kind: NodeKind::Function,
        span: (0, 0, 0, 0),
        is_exported: false,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: vec![],
        owner_class: None,
        content_hash: 0,
    }
}

/// Route handler LocalGraph — contains a `RawRoute` so Pass 1.5 emits a
/// Route node. The handler function name must match `handler`.
fn route_graph(rel_path: &str, method: &str, route_path: &str, handler: &str) -> LocalGraph {
    LocalGraph {
        file_path: rel_path.into(),
        content_hash: [0; 8],
        nodes: vec![raw_node(handler)],
        documents: vec![],
        imports: vec![],
        routes: vec![ecp_core::analyzer::types::RawRoute {
            method: method.into(),
            path: route_path.into(),
            handler: Some(handler.into()),
            span: (0, 0, 0, 0),
        }],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        call_metas: vec![],
        raw_function_metas: vec![],
    }
}

/// Consumer LocalGraph — no routes. The actual HTTP client call text lives
/// in the file written to disk; only a Function node is needed here so the
/// builder has a `source` node to attach the Fetches edge to.
fn consumer_graph(rel_path: &str) -> LocalGraph {
    LocalGraph {
        file_path: rel_path.into(),
        content_hash: [0; 8],
        nodes: vec![raw_node("consumerFn")],
        documents: vec![],
        imports: vec![],
        routes: vec![],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        call_metas: vec![],
        raw_function_metas: vec![],
    }
}

fn resolve(graph: &ecp_core::graph::ZeroCopyGraph, sref: ecp_core::pool::StrRef) -> String {
    let start = sref.offset as usize;
    let end = start + sref.len as usize;
    std::str::from_utf8(&graph.string_pool[start..end])
        .expect("utf-8")
        .to_string()
}

fn fetches_edges(graph: &ecp_core::graph::ZeroCopyGraph) -> Vec<&ecp_core::graph::Edge> {
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Fetches)
        .collect()
}

// ─── Test 1: TS exact-path match → confidence 0.8 ───────────────────────────

#[test]
fn ts_exact_match_confidence_0_8() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("client.ts"),
        "async function load() { const data = await fetch('/api/users').json(); }",
    )
    .unwrap();

    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(route_graph("api.ts", "GET", "/api/users", "getUsers"));
    builder.add_graph(consumer_graph("client.ts"));
    let graph = builder.build();

    let edges = fetches_edges(&graph);
    assert_eq!(
        edges.len(),
        1,
        "expected 1 Fetches edge for exact TS match; got {} (all edges: {:?})",
        edges.len(),
        graph
            .edges
            .iter()
            .map(|e| format!("{:?}", e.rel_type))
            .collect::<Vec<_>>()
    );
    let edge = edges[0];
    assert!(
        (edge.confidence - 0.8).abs() < 1e-6,
        "exact-path match must have confidence 0.8; got {}",
        edge.confidence
    );
    assert_eq!(
        graph.nodes[edge.target as usize].kind,
        NodeKind::Route,
        "Fetches edge must target a Route node"
    );
    let reason = resolve(&graph, edge.reason);
    assert!(
        reason.starts_with("fetch-url-match"),
        "reason must start with fetch-url-match; got {reason}"
    );
}

// ─── Test 2: Python exact-path match → confidence 0.8 ───────────────────────

#[test]
fn python_exact_match_confidence_0_8() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("client.py"),
        "import requests\ndef load_users():\n    return requests.get('/api/users').json()",
    )
    .unwrap();

    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(route_graph("app.py", "GET", "/api/users", "get_users"));
    builder.add_graph(consumer_graph("client.py"));
    let graph = builder.build();

    let edges = fetches_edges(&graph);
    assert_eq!(
        edges.len(),
        1,
        "expected 1 Fetches edge for exact Python match; got {}",
        edges.len()
    );
    assert!(
        (edges[0].confidence - 0.8).abs() < 1e-6,
        "exact-path match must have confidence 0.8; got {}",
        edges[0].confidence
    );
    assert_eq!(graph.nodes[edges[0].target as usize].kind, NodeKind::Route);
}

// ─── Test 3: TS templated match → confidence 0.6 ────────────────────────────
//
// Confidence 0.6 fires when at least one side of the match has a `:*` segment
// after normalization. The client URL must itself use a colon-param literal
// (e.g. `axios.get('/api/users/:id')`) because `fetch(\`/api/users/${id}\`)`
// is dropped by the regex (non-static URL). A static literal like
// `/api/users/42` does NOT match `/api/users/:id` (different normalized forms).

#[test]
fn ts_templated_match_confidence_0_6() {
    let tmp = tempfile::tempdir().unwrap();
    // Client uses a colon-param literal; route also has `:id`.
    // Both normalize to `api/users/:*` — match, is_templated = true on both sides.
    std::fs::write(
        tmp.path().join("client.ts"),
        "async function load(id) { return axios.get('/api/users/:id').json(); }",
    )
    .unwrap();

    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(route_graph("api.ts", "GET", "/api/users/:id", "getUser"));
    builder.add_graph(consumer_graph("client.ts"));
    let graph = builder.build();

    let edges = fetches_edges(&graph);
    assert_eq!(
        edges.len(),
        1,
        "expected 1 Fetches edge for templated match; got {}",
        edges.len()
    );
    assert!(
        (edges[0].confidence - 0.6).abs() < 1e-6,
        "templated match must have confidence 0.6; got {}",
        edges[0].confidence
    );
}

// ─── Test 4: Method mismatch → 0 edges ──────────────────────────────────────

#[test]
fn method_mismatch_zero_edges() {
    let tmp = tempfile::tempdir().unwrap();
    // Client makes a POST; route only handles GET.
    std::fs::write(
        tmp.path().join("client.ts"),
        r#"async function create() { return fetch('/api/users', { 'method': 'POST' }).json(); }"#,
    )
    .unwrap();

    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(route_graph("api.ts", "GET", "/api/users", "getUsers"));
    builder.add_graph(consumer_graph("client.ts"));
    let graph = builder.build();

    assert_eq!(
        fetches_edges(&graph).len(),
        0,
        "POST client vs GET route must produce 0 Fetches edges"
    );
}

// ─── Test 5: Cross-repo miss → 0 edges, no panic ────────────────────────────

#[test]
fn cross_repo_miss_zero_edges_no_panic() {
    let tmp = tempfile::tempdir().unwrap();
    // External host URL — no matching Route node in this graph.
    std::fs::write(
        tmp.path().join("client.ts"),
        r#"async function load() { return fetch('https://external.com/api/users').json(); }"#,
    )
    .unwrap();

    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(route_graph("api.ts", "GET", "/api/users", "getUsers"));
    builder.add_graph(consumer_graph("client.ts"));
    // Must not panic; misses are silently skipped.
    let graph = builder.build();

    assert_eq!(
        fetches_edges(&graph).len(),
        0,
        "external-host URL must produce 0 Fetches edges (cross-repo miss)"
    );
}

// ─── Test 6: Query-string stripped → 1 edge ─────────────────────────────────

#[test]
fn query_string_stripped_matches_route() {
    let tmp = tempfile::tempdir().unwrap();
    // Client fetches with query string; route path has no query string.
    std::fs::write(
        tmp.path().join("client.ts"),
        r#"async function load() { return fetch('/api/users?page=2').json(); }"#,
    )
    .unwrap();

    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(route_graph("api.ts", "GET", "/api/users", "getUsers"));
    builder.add_graph(consumer_graph("client.ts"));
    let graph = builder.build();

    let edges = fetches_edges(&graph);
    assert_eq!(
        edges.len(),
        1,
        "query-string should be stripped before matching; got {} edges",
        edges.len()
    );
    assert!(
        (edges[0].confidence - 0.8).abs() < 1e-6,
        "exact match after query-string strip must have confidence 0.8; got {}",
        edges[0].confidence
    );
}
