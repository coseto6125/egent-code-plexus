use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, RelType, ZeroCopyGraph};

pub fn build_graph(locals: impl IntoIterator<Item = LocalGraph>) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for local in locals {
        builder.add_graph(local);
    }
    builder.build()
}

pub fn closure_references(graph: &ZeroCopyGraph) -> impl Iterator<Item = &Edge> {
    graph.edges.iter().filter(|edge| {
        edge.rel_type == RelType::References
            && graph.nodes[edge.target as usize]
                .name
                .resolve(graph.string_pool.as_slice())
                .starts_with("<anonymous:")
    })
}

pub fn assert_enclosing_reachable(local: LocalGraph) {
    let graph = build_graph([local]);
    let pool = graph.string_pool.as_slice();
    let node_id = |name: &str| {
        graph
            .nodes
            .iter()
            .position(|node| node.name.resolve(pool) == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let target = node_id("target");
    let enclosing = node_id("enclosing");
    let mut reached = vec![false; graph.nodes.len()];
    let mut pending = vec![target];
    reached[target] = true;
    while let Some(node) = pending.pop() {
        for edge in graph.edges.iter().filter(|edge| {
            edge.target as usize == node
                && matches!(edge.rel_type, RelType::Calls | RelType::References)
        }) {
            let source = edge.source as usize;
            if !reached[source] {
                reached[source] = true;
                pending.push(source);
            }
        }
    }
    assert!(
        reached[enclosing],
        "enclosing must be reachable upstream from target; edges: {:?}",
        graph
            .edges
            .iter()
            .map(|edge| (
                graph.nodes[edge.source as usize].name.resolve(pool),
                edge.rel_type,
                graph.nodes[edge.target as usize].name.resolve(pool),
            ))
            .collect::<Vec<_>>()
    );
    assert!(closure_references(&graph).next().is_some());
}
