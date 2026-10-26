//! Unit tests for `post_process::sql_table_edges::emit_edges`.
//!
//! Validates cross-language table resolution: a Python function referencing a
//! SQL table (Class node emitted by the SQL DDL parser) produces a QueriesTable
//! edge without going through the SymbolTable language barrier.

use ecp_analyzer::post_process::sql_table_edges;
use ecp_analyzer::resolution::index::SymbolTable;
use ecp_core::analyzer::types::{LocalGraph, RawNode, RawSqlRef, SqlVerb};
use ecp_core::contracts::raw_records::{FileOrd, RawIndexMap};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use std::path::PathBuf;

fn raw_node(name: &str, kind: NodeKind) -> RawNode {
    RawNode {
        name: name.to_string(),
        kind,
        span: (1, 0, 1, 40),
        is_exported: false,
        heritage: Vec::new(),
        type_annotation: None,
        decorators: Vec::new(),
        calls: Vec::new(),
        field_reads: Vec::new(),
        owner_class: None,
        content_hash: 0,
    }
}

fn empty_lg(path: &str, nodes: Vec<RawNode>) -> LocalGraph {
    LocalGraph {
        file_path: PathBuf::from(path),
        content_hash: [0u8; 8],
        nodes,
        documents: Vec::new(),
        imports: Vec::new(),
        routes: Vec::new(),
        route_receivers: None,
        framework_refs: Vec::new(),
        fanout_refs: Vec::new(),
        blind_spots: Vec::new(),
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        path_literals: None,
        sql_refs: None,
        call_metas: Vec::new(),
        raw_function_metas: vec![],
        export_aliases: None,
        parsed_language: ecp_core::contracts::routing::ParsedLanguage::ABSENT,
    }
}

/// Build SymbolTable + StringPool + Node vec from a slice of LocalGraphs.
/// Mirrors the pattern in `class_membership.rs` and `enum_variant_defines.rs`.
/// Returns (symbol_table, string_pool, nodes); nodes holds the graph-level Node
/// structs whose indices match the SymbolTable-registered ids.
fn build_setup(local_graphs: &[LocalGraph]) -> (SymbolTable, StringPool, Vec<Node>) {
    let mut symbol_table = SymbolTable::new();
    let mut string_pool = StringPool::new();
    let mut nodes: Vec<Node> = Vec::new();

    for lg in local_graphs {
        let path_str = lg.file_path.to_string_lossy().replace('\\', "/");
        for rn in &lg.nodes {
            let node_id = nodes.len() as u32;
            symbol_table.register_node(&path_str, &rn.name, node_id, rn.kind);
            let name_ref = string_pool.add(&rn.name);
            let owner_ref = string_pool.add(rn.owner_class.as_deref().unwrap_or(""));
            nodes.push(Node {
                uid: 0,
                name: name_ref,
                file_idx: 0,
                kind: rn.kind,
                span: rn.span,
                community_id: 0,
                owner_class: owner_ref,
                content_hash: 0,
            });
        }
    }

    (symbol_table, string_pool, nodes)
}

/// The build's map: every raw node of every graph, in input order, as
/// `build_setup` numbers them.
fn raw_map(local_graphs: &[LocalGraph]) -> RawIndexMap {
    RawIndexMap::new(0, local_graphs.iter().map(|lg| lg.nodes.len())).unwrap()
}

/// Happy path: Python function `list_channels` references SQL table `channels`
/// (a Class node from schema.sql). Should produce exactly 1 QueriesTable edge.
#[test]
fn test_emit_edges_cross_lang_produces_queries_table_edge() {
    // Two LocalGraphs: the Python source + the SQL DDL (just for node registration).
    let py_lg = {
        let mut lg = empty_lg(
            "api/channels.py",
            vec![raw_node("list_channels", NodeKind::Function)],
        );
        lg.sql_refs = Some(Box::new([RawSqlRef {
            tables: vec![("channels".to_string(), SqlVerb::Read)],
            unresolved: false,
            span: (1, 0, 1, 40),
            enclosing_symbol: Some("list_channels".to_string()),
            enclosing_owner: None,
            endpoint: None,
        }]));
        lg
    };

    let sql_lg = empty_lg("schema.sql", vec![raw_node("channels", NodeKind::Class)]);

    let local_graphs = vec![py_lg, sql_lg];
    let (_, mut string_pool, nodes) = build_setup(&local_graphs);
    let mut edges: Vec<Edge> = Vec::new();
    let mut file_edges: Vec<(FileOrd, Edge)> = Vec::new();

    let count = sql_table_edges::emit_edges(
        &local_graphs,
        &raw_map(&local_graphs),
        &mut string_pool,
        &nodes,
        &mut edges,
        &mut file_edges,
    );

    // One edge expected.
    assert_eq!(count, 1, "emit_edges should return 1");
    assert_eq!(edges.len(), 1);

    let e = &edges[0];
    assert_eq!(e.rel_type, RelType::QueriesTable);

    // source = list_channels (idx 0, first node registered)
    assert_eq!(e.source, 0, "source should be list_channels (idx 0)");
    // target = channels Class node (idx 1, second node registered)
    assert_eq!(e.target, 1, "target should be channels Class (idx 1)");

    // reason resolves to "read"
    let reason_str = string_pool.resolve(&e.reason);
    assert_eq!(reason_str, "read");
}

/// Unresolved refs must be skipped — no fabricated edge, no panic.
#[test]
fn test_emit_edges_unresolved_skipped() {
    let mut py_lg = empty_lg(
        "api/channels.py",
        vec![raw_node("list_channels", NodeKind::Function)],
    );
    py_lg.sql_refs = Some(Box::new([RawSqlRef {
        tables: vec![("channels".to_string(), SqlVerb::Read)],
        unresolved: true, // <-- mark as unresolved
        span: (1, 0, 1, 40),
        enclosing_symbol: Some("list_channels".to_string()),
        enclosing_owner: None,
        endpoint: None,
    }]));

    let sql_lg = empty_lg("schema.sql", vec![raw_node("channels", NodeKind::Class)]);
    let local_graphs = vec![py_lg, sql_lg];
    let (_, mut string_pool, nodes) = build_setup(&local_graphs);
    let mut edges: Vec<Edge> = Vec::new();
    let mut file_edges: Vec<(FileOrd, Edge)> = Vec::new();

    let count = sql_table_edges::emit_edges(
        &local_graphs,
        &raw_map(&local_graphs),
        &mut string_pool,
        &nodes,
        &mut edges,
        &mut file_edges,
    );
    assert_eq!(count, 0);
    assert!(edges.is_empty());
}

/// Table name not in the Class-node index → drop, no fabrication.
#[test]
fn test_emit_edges_unknown_table_dropped() {
    let mut py_lg = empty_lg(
        "api/orders.py",
        vec![raw_node("get_orders", NodeKind::Function)],
    );
    py_lg.sql_refs = Some(Box::new([RawSqlRef {
        tables: vec![("nonexistent_table".to_string(), SqlVerb::Read)],
        unresolved: false,
        span: (2, 0, 2, 50),
        enclosing_symbol: Some("get_orders".to_string()),
        enclosing_owner: None,
        endpoint: None,
    }]));

    // No Class node for "nonexistent_table".
    let local_graphs = vec![py_lg];
    let (_, mut string_pool, nodes) = build_setup(&local_graphs);
    let mut edges: Vec<Edge> = Vec::new();
    let mut file_edges: Vec<(FileOrd, Edge)> = Vec::new();

    let count = sql_table_edges::emit_edges(
        &local_graphs,
        &raw_map(&local_graphs),
        &mut string_pool,
        &nodes,
        &mut edges,
        &mut file_edges,
    );
    assert_eq!(count, 0);
    assert!(edges.is_empty());
}

/// Write verb produces reason = "write".
#[test]
fn test_emit_edges_write_verb_reason() {
    let mut py_lg = empty_lg(
        "api/insert.py",
        vec![raw_node("create_user", NodeKind::Function)],
    );
    py_lg.sql_refs = Some(Box::new([RawSqlRef {
        tables: vec![("users".to_string(), SqlVerb::Write)],
        unresolved: false,
        // Inside `create_user` (1, 0, 1, 40): the endpoint is bound by span.
        span: (1, 5, 1, 30),
        enclosing_symbol: Some("create_user".to_string()),
        enclosing_owner: None,
        endpoint: None,
    }]));

    let sql_lg = empty_lg("schema.sql", vec![raw_node("users", NodeKind::Class)]);
    let local_graphs = vec![py_lg, sql_lg];
    let (_, mut string_pool, nodes) = build_setup(&local_graphs);
    let mut edges: Vec<Edge> = Vec::new();
    let mut file_edges: Vec<(FileOrd, Edge)> = Vec::new();

    let count = sql_table_edges::emit_edges(
        &local_graphs,
        &raw_map(&local_graphs),
        &mut string_pool,
        &nodes,
        &mut edges,
        &mut file_edges,
    );
    assert_eq!(count, 1);
    let reason_str = string_pool.resolve(&edges[0].reason);
    assert_eq!(reason_str, "write");
}

/// Module-top-level SQL binds the file (C4 `RecordKind::SqlRef` file
/// fallback). The edge waits in `file_edges` for the builder to give it the
/// `File` node as its source; before C4 the record was dropped.
#[test]
fn test_emit_edges_module_top_sql_binds_file() {
    let mut py_lg = empty_lg("scripts/migrate.py", vec![]);
    py_lg.sql_refs = Some(Box::new([RawSqlRef {
        tables: vec![("migrations".to_string(), SqlVerb::Write)],
        unresolved: false,
        span: (1, 0, 1, 30),
        enclosing_symbol: None, // module top-level
        enclosing_owner: None,
        endpoint: None,
    }]));

    let sql_lg = empty_lg("schema.sql", vec![raw_node("migrations", NodeKind::Class)]);
    let local_graphs = vec![py_lg, sql_lg];
    let (_, mut string_pool, nodes) = build_setup(&local_graphs);
    let mut edges: Vec<Edge> = Vec::new();
    let mut file_edges: Vec<(FileOrd, Edge)> = Vec::new();

    let count = sql_table_edges::emit_edges(
        &local_graphs,
        &raw_map(&local_graphs),
        &mut string_pool,
        &nodes,
        &mut edges,
        &mut file_edges,
    );
    assert_eq!(count, 1);
    assert!(
        edges.is_empty(),
        "a file-sourced edge waits for the File node"
    );
    let [(file, edge)] = file_edges.as_slice() else {
        panic!("one file-sourced edge expected: {file_edges:?}");
    };
    assert_eq!(*file, FileOrd(0), "the source is migrate.py's File node");
    assert_eq!(edge.rel_type, RelType::QueriesTable);
    assert_eq!(edge.target, 0, "target is the migrations Class (slot 0)");
    assert_eq!(string_pool.resolve(&edge.reason), "write");
}
