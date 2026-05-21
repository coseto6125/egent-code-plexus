//! File-kind node invariants.
//!
//! For every `NodeKind::File` node, `files[node.file_idx].path` must equal the
//! path encoded in the node's UID (`File:<path>`). Violating this invariant
//! makes downstream consumers (`ecp inspect` impact_upstream_1hop,
//! `routes.rs` category lookup) report the wrong file for a File node.
//!
//! The bug it pins: `let node_file_idx = file_node_idx.len() as u32;` in the
//! builder's File-node emission loop used a HashMap length as a proxy for the
//! iteration index. When `self.local_graphs` contained duplicate paths (which
//! does happen in practice — measured 699/3470 mismatched File nodes on the
//! `egent-code-plexus` index), `.len()` lagged behind the iteration index and every
//! File node after the first duplicate received a file_idx pointing at the
//! wrong `files[]` entry.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;

fn raw_fn(name: &str) -> RawNode {
    RawNode {
        name: name.to_string(),
        kind: NodeKind::Function,
        span: (1, 0, 5, 0),
        is_exported: false,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: vec![],
    }
}

fn local_graph(path: &str, fn_name: &str, content_byte: u8) -> LocalGraph {
    LocalGraph {
        file_path: path.into(),
        content_hash: [content_byte; 8],
        nodes: vec![raw_fn(fn_name)],
        documents: vec![],
        imports: vec![],
        routes: vec![],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
    }
}

fn assert_file_nodes_self_reference(g: &ecp_core::graph::ZeroCopyGraph) {
    let pool = g.string_pool.as_slice();
    let mut mismatches: Vec<(usize, u32, String, String)> = Vec::new();
    for (idx, n) in g.nodes.iter().enumerate() {
        if !matches!(n.kind, NodeKind::File) {
            continue;
        }
        let uid = n.uid.resolve(pool);
        let expected = uid.strip_prefix("File:").unwrap_or(uid);
        let actual = g.files[n.file_idx as usize].path.resolve(pool);
        if actual != expected {
            mismatches.push((idx, n.file_idx, expected.to_string(), actual.to_string()));
        }
    }
    assert!(
        mismatches.is_empty(),
        "File-kind nodes with mis-mapped file_idx:\n{}",
        mismatches
            .iter()
            .map(|(i, fi, e, a)| format!("  node[{i}] file_idx={fi} expected={e} resolved={a}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn file_node_idx_unique_paths_self_reference() {
    let mut b = GraphBuilder::new();
    for i in 0..5u8 {
        b.add_graph(local_graph(
            &format!("src/mod_{i}.rs"),
            &format!("fn_{i}"),
            i,
        ));
    }
    assert_file_nodes_self_reference(&b.build());
}

#[test]
fn file_node_idx_duplicate_paths_still_self_reference() {
    // Real-world scenario: the producer (scanner / hook) submits the same
    // file_path twice — different content_hashes, different RawNodes, but the
    // same path. `local_graphs.len()` File nodes still get emitted; each must
    // point at a `files[]` entry whose path matches its own UID.
    //
    // Builder sorts `local_graphs` by path before pass 1, so a duplicate placed
    // alphabetically early in the input is what makes the `file_node_idx.len()`
    // lag observable in later File-node emissions.
    let mut b = GraphBuilder::new();
    b.add_graph(local_graph("src/aaa.rs", "fn_aaa", 1));
    b.add_graph(local_graph("src/aaa.rs", "fn_aaa_dup", 2));
    b.add_graph(local_graph("src/mmm.rs", "fn_mmm", 3));
    b.add_graph(local_graph("src/zzz.rs", "fn_zzz", 4));
    assert_file_nodes_self_reference(&b.build());
}
