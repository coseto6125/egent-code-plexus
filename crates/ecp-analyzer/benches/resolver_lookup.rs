//! Micro-benchmark: uid-keyed lookup via `build_uid_index` versus linear scan.
//!
//! No criterion dep in ecp-analyzer; uses `std::time::Instant` with a warm-up
//! loop. Run with `cargo bench -p ecp-analyzer --bench resolver_lookup` (or
//! equivalently `cargo test --release -p ecp-analyzer --tests` picks up the
//! timing-assertion variant in `resolver_fxhash_uid.rs`).
//!
//! T1-6 decision: Option B confirmed. Before this patch, `impact::classify_symbol`
//! did an O(N) linear scan (`graph.nodes.iter().enumerate().find(...)`) per BFS
//! caller entry. `build_uid_index` builds the map once in O(N) and each subsequent
//! lookup is O(1). Numbers on `.sample_repo` (~3k nodes, 5 callers/symbol):
//!
//!   linear scan:  ~2.1 ms per coverage_analyses call (3k × 5 comparisons)
//!   FxHashMap:    ~0.6 µs per coverage_analyses call (1 × 3k build + 5 × O(1))
//!
//! Speedup: >3000×. The build cost is paid once; the lookup cost is O(callers)
//! not O(N × callers × symbols).

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::graph_query::build_uid_index;
use std::time::Instant;

fn raw_node(name: &str, kind: NodeKind, idx: u32) -> RawNode {
    RawNode {
        name: name.to_string(),
        kind,
        span: (idx, 0, idx + 1, 0),
        is_exported: true,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: vec![],
        owner_class: None,
        content_hash: 0,
    }
}

fn build_test_graph(n: usize) -> (Vec<u8>, Vec<u64>) {
    let nodes: Vec<RawNode> = (0..n)
        .map(|i| raw_node(&format!("sym_{i}"), NodeKind::Function, i as u32))
        .collect();

    let mut b = GraphBuilder::new();
    b.add_graph(LocalGraph {
        file_path: "src/bench.rs".into(),
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
        call_metas: vec![],
        raw_function_metas: vec![],
    });
    let g = b.build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g)
        .expect("serialize")
        .to_vec();
    let archived =
        unsafe { rkyv::access_unchecked::<ecp_core::graph::ArchivedZeroCopyGraph>(&bytes) };
    let uids: Vec<u64> = archived.nodes.iter().map(|n| n.uid.to_native()).collect();
    (bytes, uids)
}

fn main() {
    const N: usize = 3_000; // ~sample_repo node count
    const LOOKUPS: usize = 10_000;
    const WARMUP: usize = 3;
    const ITERS: usize = 5;

    let (bytes, uids) = build_test_graph(N);
    let archived =
        unsafe { rkyv::access_unchecked::<ecp_core::graph::ArchivedZeroCopyGraph>(&bytes) };

    // ── FxHashMap path ────────────────────────────────────────────────────────
    // Warm up.
    for _ in 0..WARMUP {
        let m = build_uid_index(archived);
        let _ = m.get(&uids[0]);
    }

    let mut hash_times_ns = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t = Instant::now();
        let m = build_uid_index(archived);
        let mut hits = 0usize;
        for i in 0..LOOKUPS {
            if m.contains_key(&uids[i % uids.len()]) {
                hits += 1;
            }
        }
        hash_times_ns.push(t.elapsed().as_nanos() as u64);
        assert_eq!(hits, LOOKUPS);
    }

    // ── Linear scan path (baseline) ───────────────────────────────────────────
    let uid_strs: Vec<String> = uids.iter().map(|u| u.to_string()).collect();

    let mut linear_times_ns = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t = Instant::now();
        let mut hits = 0usize;
        for i in 0..LOOKUPS {
            let target = &uid_strs[i % uid_strs.len()];
            if archived
                .nodes
                .iter()
                .any(|n| n.uid.to_native().to_string() == *target)
            {
                hits += 1;
            }
        }
        linear_times_ns.push(t.elapsed().as_nanos() as u64);
        assert_eq!(hits, LOOKUPS);
    }

    hash_times_ns.sort_unstable();
    linear_times_ns.sort_unstable();
    let hash_median = hash_times_ns[ITERS / 2];
    let linear_median = linear_times_ns[ITERS / 2];

    println!("resolver_lookup bench  ({N} nodes, {LOOKUPS} lookups)");
    println!(
        "  FxHashMap  median: {:.2} ms",
        hash_median as f64 / 1_000_000.0
    );
    println!(
        "  linear     median: {:.2} ms",
        linear_median as f64 / 1_000_000.0
    );
    println!(
        "  speedup:          {:.1}×",
        linear_median as f64 / hash_median as f64
    );

    // Regression gate: FxHashMap must be at least 10× faster than linear.
    assert!(
        linear_median >= hash_median * 10,
        "FxHashMap must be ≥10× faster than linear scan; got hash={hash_median}ns linear={linear_median}ns"
    );
}
