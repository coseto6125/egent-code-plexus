//! Shared helpers for `decorates_emission` tests.
//! Provides symbol-table build, resolver construction, and edge-filter helpers.
#![allow(dead_code)]

use ecp_analyzer::post_process::decorates_edges;
use ecp_analyzer::resolution::index::SymbolTable;
use ecp_analyzer::resolution::resolver::Resolver;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;

pub fn build_symbol_table(local_graphs: &[LocalGraph]) -> SymbolTable {
    let mut st = SymbolTable::new();
    let mut current = 0u32;
    for lg in local_graphs {
        let path_str = lg.file_path.to_string_lossy().replace('\\', "/");
        for rn in &lg.nodes {
            st.register_node(&path_str, &rn.name, current, rn.kind);
            current += 1;
        }
    }
    st
}

/// Run `decorates_edges::emit_edges` and return `(edges, synthetic_nodes, string_pool)`.
///
/// `edges` contains only `RelType::Decorates` edges.
/// `synthetic_nodes` is the slice of nodes appended beyond the initial count.
/// The returned `StringPool` owns all interned strings and must be used to
/// resolve `Node.name` on `synthetic_nodes`.
pub fn run_decorates(local_graphs: &[LocalGraph]) -> (Vec<Edge>, Vec<Node>, StringPool, usize) {
    let st = build_symbol_table(local_graphs);
    let resolver = Resolver::new(&st);
    let mut sp = StringPool::new();

    // Pre-seed nodes with one default per raw node so that source indices in
    // emitted edges (graph_base_idx + raw_idx) map correctly.
    let total_raw: usize = local_graphs.iter().map(|lg| lg.nodes.len()).sum();
    let mut nodes: Vec<Node> = (0..total_raw).map(|_| Node::default()).collect();
    let initial_count = nodes.len();

    let mut edges: Vec<Edge> = Vec::new();
    decorates_edges::emit_edges(local_graphs, &resolver, &mut sp, &mut nodes, &mut edges);

    let decorates_only: Vec<Edge> = edges
        .into_iter()
        .filter(|e| matches!(e.rel_type, RelType::Decorates))
        .collect();

    let synthetic: Vec<Node> = nodes.into_iter().skip(initial_count).collect();
    (decorates_only, synthetic, sp, initial_count)
}

/// True when `edges` contains a `Decorates` edge from `source_idx` to a
/// synthetic `Annotation` node whose name equals `annotation_name`.
pub fn has_synthetic_edge(
    edges: &[Edge],
    source_idx: u32,
    synthetic_nodes: &[Node],
    initial_count: usize,
    annotation_name: &str,
    sp: &StringPool,
) -> bool {
    edges.iter().any(|e| {
        e.source == source_idx && (e.target as usize) >= initial_count && {
            let syn_idx = e.target as usize - initial_count;
            syn_idx < synthetic_nodes.len()
                && sp.resolve(&synthetic_nodes[syn_idx].name) == annotation_name
        }
    })
}

/// True when `edges` contains a `Decorates` edge from `source_idx` to
/// `target_idx` (resolved class node).
pub fn has_resolved_edge(edges: &[Edge], source_idx: u32, target_idx: u32) -> bool {
    edges
        .iter()
        .any(|e| e.source == source_idx && e.target == target_idx)
}

pub fn has_decorator(node: &RawNode, d: &str) -> bool {
    node.decorators.iter().any(|dec| dec.contains(d))
}

/// Make a minimal `LocalGraph` with one node of the given kind/name/decorators.
pub fn single_node_graph(
    path: &str,
    kind: NodeKind,
    name: &str,
    decorators: Vec<String>,
) -> LocalGraph {
    LocalGraph {
        file_path: path.into(),
        nodes: vec![RawNode {
            name: name.into(),
            kind,
            span: (0, 0, 10, 0),
            is_exported: true,
            heritage: vec![],
            type_annotation: None,
            decorators,
            calls: vec![],
            owner_class: None,
            content_hash: 0,
        }],
        ..Default::default()
    }
}
