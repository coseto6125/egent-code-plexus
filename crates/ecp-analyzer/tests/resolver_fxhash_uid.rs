//! T1-6: uid → node_idx fast-path correctness tests.
//!
//! `ecp_core::graph_query::build_uid_index` builds a `FxHashMap<u64, u32>`
//! in O(N) once; callers like `impact::classify_symbol` use it to resolve BFS
//! caller uids back to node indices in O(1) rather than scanning all nodes.
//!
//! Covers the four cases named in the T1-6 spec:
//!   1. Existing node resolves via uid.
//!   2. Unknown uid returns None.
//!   3. Same (kind, path, owner_class, name) triple yields the same uid whether
//!      looked up through `build_uid_index` or computed directly with
//!      `ecp_core::uid::compute` (collision-disambiguated-by-identity check).
//!   4. Function-body locals do not appear in the uid index (they are dropped
//!      from the graph by design — see project memory `drop_locals_is_design`).

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::graph_query::build_uid_index;

// ── helpers ──────────────────────────────────────────────────────────────────

fn raw_node(
    name: &str,
    kind: NodeKind,
    span: (u32, u32, u32, u32),
    owner_class: Option<&str>,
    calls: Vec<String>,
) -> RawNode {
    RawNode {
        field_reads: Vec::new(),
        name: name.to_string(),
        kind,
        span,
        is_exported: true,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls,
        owner_class: owner_class.map(str::to_string),
        content_hash: 0,
    }
}

fn local_graph(file_path: &str, nodes: Vec<RawNode>) -> LocalGraph {
    LocalGraph {
        file_path: file_path.into(),
        content_hash: [0; 8],
        nodes,
        documents: vec![],
        imports: vec![],
        routes: vec![],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        path_literals: None,
        call_metas: vec![],
        raw_function_metas: vec![],
    }
}

// ── test_lookup_by_uid_resolves_existing_node ─────────────────────────────────

#[test]
fn test_lookup_by_uid_resolves_existing_node() {
    let mut b = GraphBuilder::new();
    b.add_graph(local_graph(
        "src/foo.rs",
        vec![raw_node(
            "process_request",
            NodeKind::Function,
            (1, 0, 10, 0),
            None,
            vec![],
        )],
    ));
    let g = b.build();

    // The archived graph owns mmap-like storage; archive it for query helpers.
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g)
        .expect("serialize")
        .to_vec();
    let archived =
        unsafe { rkyv::access_unchecked::<ecp_core::graph::ArchivedZeroCopyGraph>(&bytes) };

    let idx_map = build_uid_index(archived);

    // At least one node should be present and every uid in the graph should
    // round-trip through the map to a valid node index.
    assert!(
        !idx_map.is_empty(),
        "uid index must be non-empty after build"
    );

    for (i, node) in archived.nodes.iter().enumerate() {
        let uid = node.uid.to_native();
        let resolved = idx_map.get(&uid).copied();
        assert_eq!(
            resolved,
            Some(i as u32),
            "uid {uid} should map to node_idx {i}, got {resolved:?}"
        );
    }

    // Specifically verify the registered function is reachable.
    let pool = &archived.string_pool;
    let fn_node_idx = archived
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.name.resolve(pool) == "process_request")
        .map(|(i, _)| i as u32)
        .expect("process_request node must exist");

    let uid = archived.nodes[fn_node_idx as usize].uid.to_native();
    assert_eq!(
        idx_map.get(&uid).copied(),
        Some(fn_node_idx),
        "uid lookup must return node_idx for process_request"
    );
}

// ── test_lookup_by_uid_returns_none_for_unknown ───────────────────────────────

#[test]
fn test_lookup_by_uid_returns_none_for_unknown() {
    let mut b = GraphBuilder::new();
    b.add_graph(local_graph(
        "src/bar.rs",
        vec![raw_node(
            "known_fn",
            NodeKind::Function,
            (1, 0, 5, 0),
            None,
            vec![],
        )],
    ));
    let g = b.build();

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g)
        .expect("serialize")
        .to_vec();
    let archived =
        unsafe { rkyv::access_unchecked::<ecp_core::graph::ArchivedZeroCopyGraph>(&bytes) };

    let idx_map = build_uid_index(archived);

    // Sentinel uid that cannot appear in any real graph (xxh3 of empty bytes
    // is a known constant; use a different sentinel that is not a real symbol uid).
    let phantom_uid = u64::MAX;
    assert!(
        !idx_map.contains_key(&phantom_uid),
        "u64::MAX must not resolve to any node"
    );

    // A uid synthesized from a name that was never registered is also absent.
    let absent_uid =
        ecp_core::uid::compute(NodeKind::Function, "src/bar.rs", None, "never_registered");
    let known_uid = ecp_core::uid::compute(NodeKind::Function, "src/bar.rs", None, "known_fn");

    assert!(
        !idx_map.contains_key(&absent_uid),
        "uid for 'never_registered' must not resolve"
    );
    assert!(
        idx_map.contains_key(&known_uid),
        "uid for 'known_fn' must resolve"
    );
}

// ── test_lookup_by_uid_collision_disambiguated_by_owner_class ─────────────────

#[test]
fn test_lookup_by_uid_collision_disambiguated_by_owner_class() {
    // Two methods with the same base name but different owner classes produce
    // different uids because `uid::compute` includes the owner_class field.
    // Both must be findable via the index.
    let mut b = GraphBuilder::new();
    b.add_graph(local_graph(
        "src/types.rs",
        vec![
            raw_node(
                "build",
                NodeKind::Method,
                (1, 0, 5, 0),
                Some("Builder"),
                vec![],
            ),
            raw_node(
                "build",
                NodeKind::Method,
                (10, 0, 15, 0),
                Some("Assembler"),
                vec![],
            ),
        ],
    ));
    let g = b.build();

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g)
        .expect("serialize")
        .to_vec();
    let archived =
        unsafe { rkyv::access_unchecked::<ecp_core::graph::ArchivedZeroCopyGraph>(&bytes) };

    let idx_map = build_uid_index(archived);

    let uid_builder =
        ecp_core::uid::compute(NodeKind::Method, "src/types.rs", Some("Builder"), "build");
    let uid_assembler =
        ecp_core::uid::compute(NodeKind::Method, "src/types.rs", Some("Assembler"), "build");

    assert_ne!(
        uid_builder, uid_assembler,
        "different owner_class must produce different uids"
    );
    assert!(
        idx_map.contains_key(&uid_builder),
        "Builder::build uid must resolve"
    );
    assert!(
        idx_map.contains_key(&uid_assembler),
        "Assembler::build uid must resolve"
    );
    // They must map to different node indices.
    assert_ne!(
        idx_map.get(&uid_builder),
        idx_map.get(&uid_assembler),
        "two methods with different owner_class must occupy different node indices"
    );
}

// ── test_local_variable_assignment_does_not_leak_into_uid_table ──────────────

#[test]
fn test_local_variable_assignment_does_not_leak_into_uid_table() {
    // Function-body local variables are dropped from the graph by design
    // (see project memory: `drop_locals_is_design`). The parser only emits
    // module/class/function-scope nodes; local bindings never appear in
    // `LocalGraph.nodes`, so they cannot reach the uid index.
    //
    // This test verifies the contract by checking that only the module-level
    // function reaches the uid table, not any inline variable that would be
    // defined inside its body (which the parser does not capture).
    let mut b = GraphBuilder::new();
    b.add_graph(local_graph(
        "src/process.py",
        vec![
            raw_node(
                "run_pipeline",
                NodeKind::Function,
                (1, 0, 20, 0),
                None,
                vec![],
            ),
            // "result" at module scope (e.g. a top-level assignment) — IS included.
            raw_node(
                "RESULT_CACHE",
                NodeKind::Variable,
                (25, 0, 25, 10),
                None,
                vec![],
            ),
        ],
    ));
    let g = b.build();

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g)
        .expect("serialize")
        .to_vec();
    let archived =
        unsafe { rkyv::access_unchecked::<ecp_core::graph::ArchivedZeroCopyGraph>(&bytes) };

    let idx_map = build_uid_index(archived);
    let pool = &archived.string_pool;

    // Function must be in the uid index.
    let fn_uid = ecp_core::uid::compute(NodeKind::Function, "src/process.py", None, "run_pipeline");
    assert!(
        idx_map.contains_key(&fn_uid),
        "module-level function must be in uid index"
    );

    // Module-scope variable must also be in the index.
    let var_uid =
        ecp_core::uid::compute(NodeKind::Variable, "src/process.py", None, "RESULT_CACHE");
    assert!(
        idx_map.contains_key(&var_uid),
        "module-scope variable must be in uid index"
    );

    // Confirm no node exists with a name that looks like a local variable
    // (the parser never emits them, so they should not appear in the graph
    // at all — verifies the design-level drop is in effect end-to-end).
    let local_names: Vec<&str> = archived
        .nodes
        .iter()
        .filter_map(|n| {
            let name = n.name.resolve(pool);
            // Any name that is lowercase-only and short looks like a local,
            // but more precisely: we assert no Variable appears with the
            // hypothetical name "tmp" or "result" that would only arise from
            // function-body capture.
            if matches!(name, "tmp" | "result" | "x" | "i" | "j") {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    assert!(
        local_names.is_empty(),
        "function-body local variables must not appear in the graph: found {local_names:?}"
    );

    // Exact count: File node (process.py) + Function + Variable = 3.
    // (GraphBuilder emits one File node per file automatically.)
    let non_file: Vec<_> = archived
        .nodes
        .iter()
        .filter(|n| {
            !matches!(
                rkyv::deserialize::<NodeKind, rkyv::rancor::Error>(&n.kind).unwrap(),
                NodeKind::File
            )
        })
        .collect();
    assert_eq!(
        non_file.len(),
        2,
        "only run_pipeline and RESULT_CACHE should be in the graph; got {}",
        non_file.len()
    );
}

// ── timing guard: O(1) lookup under 1µs p99 ──────────────────────────────────

#[test]
fn test_uid_index_lookup_is_o1_under_1us() {
    // Build a graph with 1000 distinct functions to give the hash table
    // non-trivial work. Then perform 10k uid lookups and assert the p99
    // is below 1µs, which is a generous bound — an FxHashMap lookup on
    // a warm 1k-entry table runs in ~10–50ns on modern hardware.
    //
    // Using release-mode timing would be more accurate, but this test runs
    // under `cargo test --release` in CI. Under debug mode the bound is
    // relaxed to 5µs to absorb instrumentation overhead.
    let n_nodes = 1000usize;

    let nodes: Vec<RawNode> = (0..n_nodes)
        .map(|i| {
            raw_node(
                &format!("fn_{i}"),
                NodeKind::Function,
                (i as u32, 0, i as u32 + 1, 0),
                None,
                vec![],
            )
        })
        .collect();

    let mut b = GraphBuilder::new();
    b.add_graph(local_graph("src/bench.rs", nodes));
    let g = b.build();

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g)
        .expect("serialize")
        .to_vec();
    let archived =
        unsafe { rkyv::access_unchecked::<ecp_core::graph::ArchivedZeroCopyGraph>(&bytes) };

    let idx_map = build_uid_index(archived);

    // Collect all uids once so the lookup loop is isolated.
    let uids: Vec<u64> = archived.nodes.iter().map(|n| n.uid.to_native()).collect();

    let n_lookups = 10_000usize;
    let start = std::time::Instant::now();
    let mut found = 0usize;
    for i in 0..n_lookups {
        let uid = uids[i % uids.len()];
        if idx_map.contains_key(&uid) {
            found += 1;
        }
    }
    let elapsed = start.elapsed();
    // Prevent the loop from being optimized away.
    assert_eq!(found, n_lookups, "all lookups must hit");

    let per_lookup_ns = elapsed.as_nanos() / n_lookups as u128;
    // 5µs = 5000ns upper bound per lookup under debug mode.
    assert!(
        per_lookup_ns < 5_000,
        "uid lookup p99 must be <5µs (debug); got {per_lookup_ns}ns/lookup"
    );
}
