//! Concurrency invariant 4.2 — GraphBuilder ingest-order independence.
//!
//! `pass2` parallel path uses `flat_map_iter` whose worker-arrival order
//! is non-deterministic by design. The final `ZeroCopyGraph` exposed to
//! consumers (after sort-and-archive in `build()`) MUST be byte-identical
//! across runs and across input permutations.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawFanoutRef, RawFrameworkRef, RawNode};
use ecp_core::graph::{NodeKind, ZeroCopyGraph};

/// Canonical projection: every consumer-visible byte, in a deterministic
/// order. Excludes rkyv padding bytes (which are stable but not asserted)
/// and excludes timing-derived metadata.
fn canonical_hash(g: &ZeroCopyGraph) -> [u8; 32] {
    use blake3::Hasher;
    let pool = g.string_pool.as_slice();
    let mut h = Hasher::new();

    // Nodes: sort by (uid_resolved, name, kind, span, file_idx, community_id)
    let mut nodes: Vec<_> = g.nodes.iter().collect();
    nodes.sort_by_cached_key(|n| {
        let uid = n.uid.resolve(pool).to_string();
        let name = n.name.resolve(pool).to_string();
        (
            uid,
            name,
            format!("{:?}", n.kind),
            n.span,
            n.file_idx,
            n.community_id,
        )
    });
    for n in &nodes {
        h.update(n.uid.resolve(pool).as_bytes());
        h.update(n.name.resolve(pool).as_bytes());
        h.update(format!("{:?}", n.kind).as_bytes());
        h.update(&n.file_idx.to_le_bytes());
        let (a, b, c, d) = n.span;
        h.update(&a.to_le_bytes());
        h.update(&b.to_le_bytes());
        h.update(&c.to_le_bytes());
        h.update(&d.to_le_bytes());
        // community_id is Pass-3 Leiden output; include so a future change
        // to the clustering algorithm cannot silently regress the projection.
        h.update(&n.community_id.to_le_bytes());
    }

    // Edges: sort by (rel_type, source, target, resolved_reason)
    let mut edges: Vec<_> = g.edges.iter().collect();
    edges.sort_by_cached_key(|e| {
        let reason = e.reason.resolve(pool).to_string();
        (format!("{:?}", e.rel_type), e.source, e.target, reason)
    });
    for e in &edges {
        h.update(format!("{:?}", e.rel_type).as_bytes());
        h.update(&e.source.to_le_bytes());
        h.update(&e.target.to_le_bytes());
        h.update(e.reason.resolve(pool).as_bytes());
        h.update(&e.confidence.to_le_bytes());
    }

    // Files: sort by path
    let mut files: Vec<_> = g.files.iter().collect();
    files.sort_by_cached_key(|f| f.path.resolve(pool).to_string());
    for f in &files {
        h.update(f.path.resolve(pool).as_bytes());
        h.update(&f.content_hash);
        h.update(format!("{:?}", f.category).as_bytes());
    }

    h.finalize().into()
}

fn make_fixture_files() -> Vec<LocalGraph> {
    // >= 4 files so rayon workers actually compete; 8 keeps it under 1s.
    (0..8u8)
        .map(|i| LocalGraph {
            file_path: format!("src/mod_{i}.rs").into(),
            content_hash: [i; 8],
            nodes: vec![
                RawNode {
                    name: format!("Cls{i}"),
                    kind: NodeKind::Class,
                    span: (0, 0, 10, 0),
                    is_exported: true,
                    heritage: if i > 0 {
                        vec![format!("Cls{}", i - 1)]
                    } else {
                        vec![]
                    },
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                },
                RawNode {
                    name: format!("fn_{i}"),
                    kind: NodeKind::Function,
                    span: (12, 0, 20, 0),
                    is_exported: true,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: if i > 0 {
                        vec![format!("fn_{}", i - 1)]
                    } else {
                        vec![]
                    },
                },
            ],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![RawFrameworkRef {
                source_name: format!("Cls{i}"),
                target_name: format!("fn_{i}"),
                confidence: 0.9,
                reason: format!("test-fw-{i}"),
                span: (1, 0, 1, 10),
            }],
            fanout_refs: vec![RawFanoutRef {
                source_name: format!("Cls{i}"),
                candidates: vec![format!("fn_{i}")],
                base_confidence: 0.6,
                reason: format!("test-fanout-{i}"),
                span: (2, 0, 2, 5),
            }],
            blind_spots: vec![],
        })
        .collect()
}

fn hex(b: &[u8; 32]) -> String {
    blake3::Hash::from(*b).to_hex().to_string()
}

#[test]
fn graph_builder_order_independence_under_default_threads() {
    let files = make_fixture_files();

    let mut b1 = GraphBuilder::new();
    for lg in files.clone() {
        b1.add_graph(lg);
    }
    let g1 = b1.build();

    let mut reversed = files.clone();
    reversed.reverse();
    let mut b2 = GraphBuilder::new();
    for lg in reversed {
        b2.add_graph(lg);
    }
    let g2 = b2.build();

    let h1 = canonical_hash(&g1);
    let h2 = canonical_hash(&g2);
    assert_eq!(
        h1,
        h2,
        "canonical projection differs across ingest order: {} vs {}",
        hex(&h1),
        hex(&h2)
    );
}

#[test]
fn graph_builder_repeated_build_is_stable() {
    let files = make_fixture_files();
    let hashes: Vec<[u8; 32]> = (0..5)
        .map(|_| {
            let mut b = GraphBuilder::new();
            for lg in files.clone() {
                b.add_graph(lg);
            }
            canonical_hash(&b.build())
        })
        .collect();

    let first = hashes[0];
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(
            *h,
            first,
            "build run #{i} hashes differently from run #0: {} vs {}",
            hex(h),
            hex(&first)
        );
    }
}
