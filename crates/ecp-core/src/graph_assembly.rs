//! Final graph assembly: the derived half of [`ZeroCopyGraph`].
//!
//! A graph's fields split in two. `nodes`, `edges`, and the sparse side
//! tables are *given* — a builder decides what is in them. `out_offsets`,
//! `in_offsets`, `in_edge_idx`, `name_index`, `kind_offsets`,
//! `kind_node_idx` and the edge order they index into are *derived* — there
//! is exactly one correct value for each, given the nodes and edges.
//!
//! [`GraphAssembly::finish`] owns that derivation so no caller has to know
//! that edges are stored source-sorted, that `in_edge_idx` is a permutation
//! rather than an offset table, that `name_index` is hash-sorted and skips
//! tombstones, or that sorting the edges invalidates every pre-existing
//! `CallMeta::edge_idx`. Callers hand over what they know; the invariants
//! are applied once, here.

use crate::graph::{
    BlindSpotRecord, CallMeta, Edge, File, FunctionMeta, NameIndexEntry, Node, NodeKind,
    RouteShape, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use crate::pool::StringPool;

/// The given half of a [`ZeroCopyGraph`], plus the sparse side tables.
///
/// Construct it with `..Default::default()` and call [`finish`](Self::finish);
/// every field left at its default is a table the graph legitimately has
/// none of.
///
/// **Edge indices are pre-sort.** `finish` sorts `edges` by source to build
/// the outgoing CSR, so any `edge_idx` a caller holds (in `call_metas`, or
/// returned from its own bookkeeping) refers to the order it supplied.
/// `call_metas` is remapped for you; nothing else in the struct references
/// an edge index.
pub struct GraphAssembly {
    pub string_pool: StringPool,
    pub files: Vec<File>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// `edge_idx` in pre-sort space. Remapped to final positions, sorted,
    /// and deduplicated by `finish`; entries pointing past the edge list are
    /// dropped.
    pub call_metas: Vec<CallMeta>,
    /// Any order. Sorted by `node_idx` by `finish`, which also derives the
    /// dense `node_flags` mirror from it.
    pub function_metas: Vec<FunctionMeta>,
    pub blind_spots: Vec<BlindSpotRecord>,
    pub route_shapes: Vec<RouteShape>,
    /// Index of the first `NodeKind::Process` node; `traces_offsets[k+1]`
    /// bounds the trace of the k-th process after it. Given, not derived:
    /// later passes append non-Process nodes behind the Process block, so
    /// the boundary is a fact about how the caller ordered `nodes`.
    ///
    /// With no Process block the canonical value is `nodes.len()`, but the
    /// `Default` of `0` is equally safe: every consumer bounds the block by
    /// the trace count (`traces_offsets.len() - 1`), which is then zero.
    pub process_start: u32,
    pub traces_offsets: Vec<u32>,
    pub traces_data: Vec<u32>,
    pub fingerprint: [u8; 32],
}

impl Default for GraphAssembly {
    fn default() -> Self {
        Self {
            string_pool: StringPool::new(),
            files: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            call_metas: Vec::new(),
            function_metas: Vec::new(),
            blind_spots: Vec::new(),
            route_shapes: Vec::new(),
            process_start: 0,
            // `traces_offsets[k+1]` is read for every process, so the vector
            // needs its sentinel even when there are no processes.
            traces_offsets: vec![0],
            traces_data: Vec::new(),
            fingerprint: [0; 32],
        }
    }
}

impl GraphAssembly {
    /// Sort the edges, derive both CSR directions, the name index and the
    /// kind index, remap `call_metas` onto the sorted edge order, and return
    /// the finished graph.
    pub fn finish(self) -> ZeroCopyGraph {
        let Self {
            string_pool,
            files,
            nodes,
            mut edges,
            call_metas,
            mut function_metas,
            blind_spots,
            route_shapes,
            process_start,
            traces_offsets,
            traces_data,
            fingerprint,
        } = self;

        assert!(
            edges.len() as u64 <= u32::MAX as u64,
            "total edge count {} exceeds u32::MAX — edge index scheme would overflow",
            edges.len()
        );

        // One stable sort of edge indices by source. Stability makes the
        // result the exact order `edges.sort_by_key(|e| e.source)` would
        // give, so equal sources keep their insertion order.
        let mut pre_sort_to_sorted: Vec<u32> = vec![0; edges.len()];
        {
            let mut perm: Vec<u32> = (0..edges.len() as u32).collect();
            perm.sort_by_key(|&i| edges[i as usize].source);
            for (sorted_idx, &pre_idx) in perm.iter().enumerate() {
                pre_sort_to_sorted[pre_idx as usize] = sorted_idx as u32;
            }
        }

        // Remap before the permutation is applied: applying it consumes
        // `pre_sort_to_sorted` as its cycle buffer.
        let mut call_metas: Vec<CallMeta> = call_metas
            .into_iter()
            .filter_map(|m| {
                pre_sort_to_sorted
                    .get(m.edge_idx as usize)
                    .map(|&sorted_idx| CallMeta {
                        edge_idx: sorted_idx,
                        ..m
                    })
            })
            .collect();
        call_metas.sort_by_key(|m| m.edge_idx);
        call_metas.dedup_by_key(|m| m.edge_idx);

        apply_destination_permutation(&mut edges, pre_sort_to_sorted);

        let num_nodes = nodes.len();
        let mut out_offsets = vec![0u32; num_nodes + 1];
        for edge in &edges {
            out_offsets[edge.source as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            out_offsets[i + 1] += out_offsets[i];
        }

        let mut in_edge_idx: Vec<u32> = (0..edges.len() as u32).collect();
        in_edge_idx.sort_by_key(|&idx| edges[idx as usize].target);

        let mut in_offsets = vec![0u32; num_nodes + 1];
        for &idx in &in_edge_idx {
            in_offsets[edges[idx as usize].target as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            in_offsets[i + 1] += in_offsets[i];
        }

        // `function_meta()` binary-searches by node_idx; `node_flags` is its
        // dense low-byte mirror, read by the hot boolean filters. Deriving
        // both here is what keeps them from drifting apart — which needs the
        // dedup as much as the sort: with two metas on one node the binary
        // search may return either, while the mirror would hold whichever the
        // scatter wrote last. Keep the first, as `call_metas` does.
        function_metas.sort_by_key(|m| m.node_idx);
        function_metas.dedup_by_key(|m| m.node_idx);
        let mut node_flags: Vec<u8> = vec![0u8; num_nodes];
        for meta in &function_metas {
            if let Some(slot) = node_flags.get_mut(meta.node_idx as usize) {
                *slot = (meta.flags & 0x00ff) as u8;
            }
        }

        // Tombstone nodes (uid-collision survivors) carry an empty name and
        // are skipped: an empty-string lookup must not return them.
        let mut name_index: Vec<NameIndexEntry> = nodes
            .iter()
            .enumerate()
            .filter_map(|(idx, n)| {
                let name = string_pool.resolve(&n.name);
                if name.is_empty() {
                    return None;
                }
                Some(NameIndexEntry {
                    name_hash: crate::uid::xxh3_64_bytes(name.as_bytes()),
                    node_idx: idx as u32,
                })
            })
            .collect();
        name_index.sort_unstable_by_key(|e| e.name_hash);

        let kind_count = NodeKind::VARIANT_COUNT;
        let mut kind_offsets: Vec<u32> = vec![0u32; kind_count + 1];
        for n in &nodes {
            kind_offsets[n.kind.as_index() + 1] += 1;
        }
        for i in 1..kind_offsets.len() {
            kind_offsets[i] += kind_offsets[i - 1];
        }
        let mut kind_node_idx: Vec<u32> = vec![0u32; nodes.len()];
        let mut cursors: Vec<u32> = kind_offsets[..kind_count].to_vec();
        for (idx, n) in nodes.iter().enumerate() {
            let k = n.kind.as_index();
            kind_node_idx[cursors[k] as usize] = idx as u32;
            cursors[k] += 1;
        }

        ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint,
            string_pool: string_pool.bytes,
            files,
            nodes,
            edges,
            out_offsets,
            in_offsets,
            in_edge_idx,
            name_index,
            process_start,
            traces_offsets,
            traces_data,
            blind_spots,
            route_shapes,
            call_metas,
            function_metas,
            kind_offsets,
            kind_node_idx,
            node_flags,
        }
    }
}

/// Move `items[i]` to position `dest[i]` for every `i`, in place: each swap
/// settles one element, so the cost is at most `items.len()` swaps and no
/// second buffer of `T`. `dest` must be a permutation of `0..items.len()`;
/// it is consumed as the cycle bookkeeping.
fn apply_destination_permutation<T>(items: &mut [T], mut dest: Vec<u32>) {
    debug_assert_eq!(items.len(), dest.len());
    for i in 0..items.len() {
        loop {
            let d = dest[i] as usize;
            if d == i {
                break;
            }
            items.swap(i, d);
            dest.swap(i, d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::RelType;
    use crate::pool::StrRef;

    fn node(pool: &mut StringPool, name: &str, kind: NodeKind) -> Node {
        Node {
            uid: crate::uid::compute(kind, "f.rs", None, name),
            name: pool.add(name),
            file_idx: 0,
            kind,
            span: (0, 0, 0, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        }
    }

    fn edge(source: u32, target: u32) -> Edge {
        Edge {
            source,
            target,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason: StrRef::default(),
        }
    }

    #[test]
    fn finish_out_offsets_slice_the_source_sorted_edges() {
        let mut pool = StringPool::new();
        let nodes = vec![
            node(&mut pool, "a", NodeKind::Function),
            node(&mut pool, "b", NodeKind::Function),
            node(&mut pool, "c", NodeKind::Function),
        ];
        // Deliberately unsorted by source: 2→0 before 0→1.
        let g = GraphAssembly {
            string_pool: pool,
            nodes,
            edges: vec![edge(2, 0), edge(0, 1), edge(0, 2)],
            ..Default::default()
        }
        .finish();

        for src in 0..3usize {
            let start = g.out_offsets[src] as usize;
            let end = g.out_offsets[src + 1] as usize;
            for e in &g.edges[start..end] {
                assert_eq!(e.source as usize, src, "out_offsets slice mismatch");
            }
        }
        assert_eq!(g.out_offsets, vec![0, 2, 2, 3]);
    }

    #[test]
    fn finish_in_edge_idx_groups_edges_by_target() {
        let mut pool = StringPool::new();
        let nodes = vec![
            node(&mut pool, "a", NodeKind::Function),
            node(&mut pool, "b", NodeKind::Function),
        ];
        let g = GraphAssembly {
            string_pool: pool,
            nodes,
            edges: vec![edge(0, 1), edge(1, 1)],
            ..Default::default()
        }
        .finish();

        let start = g.in_offsets[1] as usize;
        let end = g.in_offsets[2] as usize;
        assert_eq!(end - start, 2);
        for &eidx in &g.in_edge_idx[start..end] {
            assert_eq!(g.edges[eidx as usize].target, 1);
        }
    }

    #[test]
    fn finish_remaps_call_meta_edge_idx_through_the_edge_sort() {
        let mut pool = StringPool::new();
        let nodes = vec![
            node(&mut pool, "a", NodeKind::Function),
            node(&mut pool, "b", NodeKind::Function),
        ];
        // Pre-sort edge 0 is 1→0, which sorts to position 1.
        let g = GraphAssembly {
            string_pool: pool,
            nodes,
            edges: vec![edge(1, 0), edge(0, 1)],
            call_metas: vec![CallMeta {
                edge_idx: 0,
                flags: CallMeta::FLAG_DYNAMIC_DISPATCH,
                dispatch_type: StrRef::default(),
            }],
            ..Default::default()
        }
        .finish();

        assert_eq!(g.call_metas.len(), 1);
        assert_eq!(g.call_metas[0].edge_idx, 1);
        assert_eq!(g.edges[1].source, 1, "remap must follow the sorted edge");
        assert!(g.call_meta(1).unwrap().is_dynamic_dispatch());
    }

    #[test]
    fn finish_keeps_equal_source_edges_in_insertion_order() {
        // The permutation and the edge sort must agree, and both must be
        // stable: a caller's `call_metas` index into the order it supplied.
        let mut pool = StringPool::new();
        let nodes = vec![
            node(&mut pool, "a", NodeKind::Function),
            node(&mut pool, "b", NodeKind::Function),
            node(&mut pool, "c", NodeKind::Function),
        ];
        let mut first = edge(0, 2);
        first.confidence = 0.5;
        let mut second = edge(0, 1);
        second.confidence = 0.25;
        let g = GraphAssembly {
            string_pool: pool,
            nodes,
            edges: vec![edge(1, 0), first, second],
            ..Default::default()
        }
        .finish();

        let start = g.out_offsets[0] as usize;
        let end = g.out_offsets[1] as usize;
        let order: Vec<(u32, f32)> = g.edges[start..end]
            .iter()
            .map(|e| (e.target, e.confidence))
            .collect();
        assert_eq!(order, vec![(2, 0.5), (1, 0.25)]);
    }

    #[test]
    fn finish_keeps_the_first_of_two_metas_on_one_slot() {
        let mut pool = StringPool::new();
        let nodes = vec![
            node(&mut pool, "a", NodeKind::Function),
            node(&mut pool, "b", NodeKind::Function),
        ];
        let meta = |flags| FunctionMeta {
            node_idx: 1,
            flags,
            params: Vec::new(),
            return_type: StrRef::default(),
            decorators: Vec::new(),
        };
        let g = GraphAssembly {
            string_pool: pool,
            nodes,
            edges: vec![edge(0, 1), edge(0, 1)],
            call_metas: vec![
                CallMeta {
                    edge_idx: 0,
                    flags: CallMeta::FLAG_DIRECT,
                    dispatch_type: StrRef::default(),
                },
                CallMeta {
                    edge_idx: 0,
                    flags: CallMeta::FLAG_CALLBACK,
                    dispatch_type: StrRef::default(),
                },
            ],
            function_metas: vec![
                meta(FunctionMeta::FLAG_TEST),
                meta(FunctionMeta::FLAG_ASYNC),
            ],
            ..Default::default()
        }
        .finish();

        assert_eq!(g.call_metas.len(), 1);
        assert!(g.call_metas[0].is_direct(), "first call meta wins");
        assert_eq!(g.function_metas.len(), 1);
        assert!(g.function_meta(1).unwrap().is_test(), "first meta wins");
        // The dense mirror must agree with what the binary search returns.
        assert_eq!(g.node_flags[1], FunctionMeta::FLAG_TEST as u8);
    }

    #[test]
    fn finish_drops_call_meta_pointing_past_the_edge_list() {
        let mut pool = StringPool::new();
        let nodes = vec![node(&mut pool, "a", NodeKind::Function)];
        let g = GraphAssembly {
            string_pool: pool,
            nodes,
            call_metas: vec![CallMeta {
                edge_idx: 7,
                flags: 0,
                dispatch_type: StrRef::default(),
            }],
            ..Default::default()
        }
        .finish();
        assert!(g.call_metas.is_empty());
    }

    #[test]
    fn finish_derives_node_flags_from_function_metas() {
        let mut pool = StringPool::new();
        let nodes = vec![
            node(&mut pool, "a", NodeKind::Function),
            node(&mut pool, "b", NodeKind::Function),
        ];
        let g = GraphAssembly {
            string_pool: pool,
            nodes,
            function_metas: vec![FunctionMeta {
                node_idx: 1,
                flags: FunctionMeta::FLAG_TEST | FunctionMeta::FLAG_ASYNC,
                params: Vec::new(),
                return_type: StrRef::default(),
                decorators: Vec::new(),
            }],
            ..Default::default()
        }
        .finish();

        assert_eq!(g.node_flags.len(), g.nodes.len());
        assert_eq!(g.node_flags[0], 0);
        assert_eq!(
            g.node_flags[1],
            (FunctionMeta::FLAG_TEST | FunctionMeta::FLAG_ASYNC) as u8
        );
        assert!(g.function_meta(1).unwrap().is_test());
    }

    #[test]
    fn finish_name_index_finds_every_named_node_and_skips_tombstones() {
        let mut pool = StringPool::new();
        let mut tombstone = node(&mut pool, "gone", NodeKind::Function);
        tombstone.name = pool.add("");
        let nodes = vec![
            node(&mut pool, "alpha", NodeKind::Function),
            tombstone,
            node(&mut pool, "alpha", NodeKind::Method),
        ];
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(
            &GraphAssembly {
                string_pool: pool,
                nodes,
                ..Default::default()
            }
            .finish(),
        )
        .unwrap();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();

        let mut hits: Vec<u32> = archived.nodes_by_name("alpha").collect();
        hits.sort_unstable(); // hash-equal entries have no guaranteed order
        assert_eq!(hits, vec![0, 2]);
        assert_eq!(archived.nodes_by_name("").count(), 0);
    }

    #[test]
    fn finish_kind_index_partitions_nodes_by_kind() {
        let mut pool = StringPool::new();
        let nodes = vec![
            node(&mut pool, "a", NodeKind::Function),
            node(&mut pool, "K", NodeKind::Class),
            node(&mut pool, "b", NodeKind::Function),
        ];
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(
            &GraphAssembly {
                string_pool: pool,
                nodes,
                ..Default::default()
            }
            .finish(),
        )
        .unwrap();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();

        let funcs: Vec<u32> = archived.nodes_by_kind(NodeKind::Function).collect();
        let classes: Vec<u32> = archived.nodes_by_kind(NodeKind::Class).collect();
        assert_eq!(funcs, vec![0, 2]);
        assert_eq!(classes, vec![1]);
    }

    type EdgeKey = (u32, u32, RelType, u32, StrRef);
    type MetaKey = (u32, u8, StrRef);

    fn edge_key(e: &Edge) -> EdgeKey {
        (
            e.source,
            e.target,
            e.rel_type,
            e.confidence.to_bits(),
            e.reason,
        )
    }

    fn meta_key(m: &CallMeta) -> MetaKey {
        (m.edge_idx, m.flags, m.dispatch_type)
    }

    /// The pre-change derivation: a stable index sort for the call-meta
    /// remap, then a second stable sort of the edges themselves.
    fn reference_two_sorts(
        mut edges: Vec<Edge>,
        call_metas: &[CallMeta],
    ) -> (Vec<EdgeKey>, Vec<MetaKey>) {
        let mut pre_sort_to_sorted: Vec<u32> = vec![0; edges.len()];
        let mut perm: Vec<usize> = (0..edges.len()).collect();
        perm.sort_by_key(|&i| edges[i].source);
        for (sorted_idx, &pre_idx) in perm.iter().enumerate() {
            pre_sort_to_sorted[pre_idx] = sorted_idx as u32;
        }
        edges.sort_by_key(|e| e.source);
        let mut metas: Vec<CallMeta> = call_metas
            .iter()
            .filter_map(|m| {
                pre_sort_to_sorted
                    .get(m.edge_idx as usize)
                    .map(|&sorted_idx| CallMeta {
                        edge_idx: sorted_idx,
                        ..m.clone()
                    })
            })
            .collect();
        metas.sort_by_key(|m| m.edge_idx);
        metas.dedup_by_key(|m| m.edge_idx);
        (
            edges.iter().map(edge_key).collect(),
            metas.iter().map(meta_key).collect(),
        )
    }

    fn assert_finish_matches_two_sorts(num_nodes: usize, edges: Vec<Edge>, metas: Vec<CallMeta>) {
        let (want_edges, want_metas) = reference_two_sorts(edges.clone(), &metas);
        let mut pool = StringPool::new();
        let nodes = (0..num_nodes)
            .map(|i| node(&mut pool, &format!("n{i}"), NodeKind::Function))
            .collect();
        let g = GraphAssembly {
            string_pool: pool,
            nodes,
            edges,
            call_metas: metas,
            ..Default::default()
        }
        .finish();

        let got_edges: Vec<EdgeKey> = g.edges.iter().map(edge_key).collect();
        assert_eq!(got_edges, want_edges, "edge order");
        assert_eq!(
            g.call_metas.iter().map(meta_key).collect::<Vec<_>>(),
            want_metas,
            "call metas"
        );

        let mut want_out = vec![0u32; num_nodes + 1];
        for e in &want_edges {
            want_out[e.0 as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            want_out[i + 1] += want_out[i];
        }
        assert_eq!(g.out_offsets, want_out, "out_offsets");

        let mut want_in_idx: Vec<u32> = (0..want_edges.len() as u32).collect();
        want_in_idx.sort_by_key(|&i| want_edges[i as usize].1);
        assert_eq!(g.in_edge_idx, want_in_idx, "in_edge_idx");
        let mut want_in = vec![0u32; num_nodes + 1];
        for e in &want_edges {
            want_in[e.1 as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            want_in[i + 1] += want_in[i];
        }
        assert_eq!(g.in_offsets, want_in, "in_offsets");
    }

    fn tagged_edge(source: u32, target: u32, tag: u32) -> Edge {
        // A distinct confidence per edge makes any reorder among equal
        // sources visible in the comparison.
        let mut e = edge(source, target);
        e.confidence = tag as f32;
        e
    }

    fn meta(edge_idx: u32, flags: u8) -> CallMeta {
        CallMeta {
            edge_idx,
            flags,
            dispatch_type: StrRef::default(),
        }
    }

    #[test]
    fn test_finish_shared_sources_interleaved_metas_matches_two_sorts() {
        // Sources drawn from {0, 2, 3, 5, 8}: many edges share a source and
        // nodes 1, 4, 6, 7 have no outgoing edges.
        const SOURCES: [u32; 5] = [0, 2, 3, 5, 8];
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (x >> 33) as u32
        };
        let edges: Vec<Edge> = (0..600)
            .map(|i| tagged_edge(SOURCES[next() as usize % 5], next() % 9, i))
            .collect();
        let mut metas: Vec<CallMeta> = Vec::new();
        for i in (0..600u32).rev().step_by(3) {
            metas.push(meta(i, CallMeta::FLAG_DIRECT));
            if i % 7 == 0 {
                // Second meta on one edge: the first one supplied must win.
                metas.push(meta(i, CallMeta::FLAG_CALLBACK));
            }
        }
        metas.push(meta(600, CallMeta::FLAG_DIRECT)); // past the edge list
        assert_finish_matches_two_sorts(9, edges, metas);
    }

    #[test]
    fn test_finish_degenerate_edge_orders_match_two_sorts() {
        let all_one_source: Vec<Edge> = (0..50).map(|i| tagged_edge(3, i % 4, i)).collect();
        let reverse_sorted: Vec<Edge> = (0..50).map(|i| tagged_edge(49 - i, i, i)).collect();
        let already_sorted: Vec<Edge> = (0..50).map(|i| tagged_edge(i / 5, 0, i)).collect();
        let metas = || -> Vec<CallMeta> {
            (0..50)
                .step_by(4)
                .map(|i| meta(i, CallMeta::FLAG_DIRECT))
                .collect()
        };
        assert_finish_matches_two_sorts(4, all_one_source, metas());
        assert_finish_matches_two_sorts(50, reverse_sorted, metas());
        assert_finish_matches_two_sorts(10, already_sorted, metas());
        assert_finish_matches_two_sorts(2, Vec::new(), vec![meta(0, CallMeta::FLAG_DIRECT)]);
    }

    #[test]
    fn test_apply_destination_permutation_cycles_moves_each_item_to_dest() {
        // Two cycles (0→2→1→0, 3↔4) and one fixed point (5).
        let mut items = vec!['a', 'b', 'c', 'd', 'e', 'f'];
        apply_destination_permutation(&mut items, vec![2, 0, 1, 4, 3, 5]);
        assert_eq!(items, vec!['b', 'c', 'a', 'e', 'd', 'f']);

        let mut empty: Vec<char> = Vec::new();
        apply_destination_permutation(&mut empty, Vec::new());
        assert!(empty.is_empty());
    }
}
