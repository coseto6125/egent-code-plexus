use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::RelType;

pub fn assert_enclosing_reachable(local: LocalGraph) {
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    let graph = builder.build();
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
}
