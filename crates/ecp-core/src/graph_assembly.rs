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

use crate::contracts::workspace::WorkspaceFacts;
use crate::graph::{
    BlindSpotRecord, CallMeta, Edge, ExportAliasFact, File, FunctionMeta, ImportFact,
    LexicalParentFact, NameIndexEntry, Node, NodeKind, PathAliasFact, RouteMiddleware, RouteShape,
    SupertypeFact, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
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
    pub module_facts: ModuleFactTables,
    /// The path aliases the index resolved with, in declaration order;
    /// stored as given.
    pub alias_table: Vec<PathAliasFact>,
    /// Any order. Sorted by `type_idx` by `finish`, which keeps the first
    /// entry of a repeated type.
    pub supertype_table: Vec<SupertypeFact>,
    /// Any order. Sorted by `node_idx` by `finish`, which keeps the first
    /// entry of a repeated Route.
    pub route_middleware: Vec<RouteMiddleware>,
    /// Contract C2's workspace facts; stored as given.
    pub workspace: WorkspaceFacts,
    /// Any order. Sorted by `node_idx` by `finish`, which keeps the first
    /// row of a repeated node.
    pub lexical_parents: Vec<LexicalParentFact>,
}

/// The import / export facts of each file and node, as the builder records
/// them (see the v13 fields of [`ZeroCopyGraph`]).
#[derive(Debug, Default)]
pub struct ModuleFactTables {
    /// CSR offsets over `files` into `imports`. Empty records no import
    /// for any file.
    pub file_imports: Vec<u32>,
    pub imports: Vec<ImportFact>,
    /// CSR offsets over `files` into `export_aliases`, like `file_imports`.
    pub file_export_aliases: Vec<u32>,
    pub export_aliases: Vec<ExportAliasFact>,
    /// One byte per file; files past its end have no flag.
    pub file_flags: Vec<u8>,
    /// Exported-node bitmap; nodes past its end are not exported.
    pub exported_nodes: Vec<u8>,
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
            module_facts: ModuleFactTables::default(),
            alias_table: Vec::new(),
            supertype_table: Vec::new(),
            route_middleware: Vec::new(),
            workspace: WorkspaceFacts::default(),
            lexical_parents: Vec::new(),
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
            module_facts,
            alias_table,
            mut supertype_table,
            mut route_middleware,
            workspace,
            mut lexical_parents,
        } = self;

        assert!(
            edges.len() as u64 <= u32::MAX as u64,
            "total edge count {} exceeds u32::MAX — edge index scheme would overflow",
            edges.len()
        );

        // Permutation first: `pre_sort_to_sorted[pre] = sorted`. Built with
        // the same stable sort `edges.sort_by_key` uses below, so equal
        // sources keep their relative order in both.
        let mut pre_sort_to_sorted: Vec<u32> = vec![0; edges.len()];
        {
            let mut perm: Vec<usize> = (0..edges.len()).collect();
            perm.sort_by_key(|&i| edges[i].source);
            for (sorted_idx, &pre_idx) in perm.iter().enumerate() {
                pre_sort_to_sorted[pre_idx] = sorted_idx as u32;
            }
        }
        edges.sort_by_key(|e| e.source);

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

        let ModuleFactTables {
            file_imports,
            imports,
            file_export_aliases,
            export_aliases,
            mut file_flags,
            mut exported_nodes,
        } = module_facts;
        let file_imports = csr_offsets(file_imports, imports.len(), files.len(), "imports");
        let file_export_aliases = csr_offsets(
            file_export_aliases,
            export_aliases.len(),
            files.len(),
            "export_aliases",
        );
        file_flags.resize(files.len(), 0);
        exported_nodes.resize(num_nodes.div_ceil(8), 0);
        let file_path_order = file_path_order(&files, &string_pool);
        // `supertypes_of()` binary-searches by `type_idx`.
        supertype_table.sort_by_key(|fact| fact.type_idx);
        supertype_table.dedup_by_key(|fact| fact.type_idx);
        // `route_middleware_of()` binary-searches by `node_idx`.
        route_middleware.sort_by_key(|entry| entry.node_idx);
        route_middleware.dedup_by_key(|entry| entry.node_idx);
        // `lexical_parent()` binary-searches by `node_idx`.
        lexical_parents.sort_by_key(|fact| fact.node_idx);
        lexical_parents.dedup_by_key(|fact| fact.node_idx);

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
            file_imports,
            imports,
            file_export_aliases,
            export_aliases,
            file_flags,
            exported_nodes,
            file_path_order,
            alias_table,
            supertype_table,
            route_middleware,
            workspace,
            lexical_parents,
        }
    }
}

/// `offsets` checked as a CSR table over `files` files into `rows` rows;
/// empty offsets with no rows give every file an empty range.
fn csr_offsets(offsets: Vec<u32>, rows: usize, files: usize, what: &str) -> Vec<u32> {
    if offsets.is_empty() && rows == 0 {
        return vec![0; files + 1];
    }
    assert!(
        offsets.len() == files + 1
            && offsets.first() == Some(&0)
            && offsets.last().map(|&o| o as usize) == Some(rows)
            && offsets.windows(2).all(|pair| pair[0] <= pair[1]),
        "{what}: {} offsets for {files} files and {rows} rows is no CSR table",
        offsets.len()
    );
    offsets
}

/// `ZeroCopyGraph::file_path_order`: empty when `files` is strictly in
/// `Path` order, else the indices sorted by path.
fn file_path_order(files: &[File], pool: &StringPool) -> Vec<u32> {
    fn path<'p>(file: &File, pool: &'p StringPool) -> &'p std::path::Path {
        std::path::Path::new(pool.resolve(&file.path))
    }
    if files
        .windows(2)
        .all(|pair| path(&pair[0], pool) < path(&pair[1], pool))
    {
        return Vec::new();
    }
    let mut order: Vec<u32> = (0..files.len() as u32).collect();
    order.sort_by(|&a, &b| path(&files[a as usize], pool).cmp(path(&files[b as usize], pool)));
    order
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

    fn file(pool: &mut StringPool, path: &str) -> File {
        File {
            path: pool.add(path),
            mtime: 0,
            content_hash: [0; 8],
            category: crate::graph::FileCategory::Source,
        }
    }

    /// The facts round-trip per file, and a file with no recorded import
    /// reads as an empty slice, not as missing. An alias of `""` stays
    /// distinct from no alias, and a row's target file index reads back as
    /// that file's path.
    #[test]
    fn test_finish_module_facts_roundtrip_per_file_keeps_none_and_empty_alias() {
        use crate::analyzer::reexport::ImportRef;
        use crate::analyzer::types::BindingKind;
        let mut pool = StringPool::new();
        let files = vec![file(&mut pool, "a.py"), file(&mut pool, "b.ts")];
        let nodes = vec![
            node(&mut pool, "x", NodeKind::Function),
            node(&mut pool, "y", NodeKind::Function),
        ];
        let imports = vec![
            ImportFact {
                source: pool.add("./m"),
                imported_name: pool.add("d"),
                alias: None,
                binding_kind: Some(BindingKind::EsDefault),
                target_file: Some(1),
            },
            ImportFact {
                source: pool.add("./m"),
                imported_name: pool.add("e"),
                alias: Some(pool.add("")),
                binding_kind: None,
                target_file: None,
            },
        ];
        let export_aliases = vec![ExportAliasFact {
            exported: pool.add("f"),
            local: pool.add("y"),
        }];
        let graph = GraphAssembly {
            string_pool: pool,
            files,
            nodes,
            module_facts: ModuleFactTables {
                file_imports: vec![0, 2, 2],
                imports,
                file_export_aliases: vec![0, 0, 1],
                export_aliases,
                file_flags: vec![0, crate::graph::FILE_FLAG_ES_MODULE],
                exported_nodes: vec![0b10],
            },
            ..Default::default()
        }
        .finish();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert!(archived.has_module_facts());
        let pool = archived.string_pool.as_slice();
        let a: Vec<ImportRef<'_>> = archived
            .file_imports(0)
            .unwrap()
            .iter()
            .map(|row| ImportRef::from_archived(row, pool, &archived.files))
            .collect();
        assert_eq!(
            a,
            [
                ImportRef {
                    source: "./m",
                    imported_name: "d",
                    alias: None,
                    binding_kind: Some(BindingKind::EsDefault),
                    target: Some("b.ts"),
                },
                ImportRef {
                    source: "./m",
                    imported_name: "e",
                    alias: Some(""),
                    binding_kind: None,
                    target: None,
                },
            ]
        );
        assert_eq!(archived.file_imports(1).map(<[_]>::len), Some(0));
        assert!(archived.file_imports(2).is_none(), "past the last file");
        let b = archived.file_export_aliases(1).unwrap();
        assert_eq!(
            (b[0].exported.resolve(pool), b[0].local.resolve(pool)),
            ("f", "y")
        );
        assert!(!archived.is_es_module_file(0) && archived.is_es_module_file(1));
        assert!(!archived.is_exported_node(0) && archived.is_exported_node(1));
        assert!(!archived.is_exported_node(64), "past the bitmap");
    }

    /// A builder that records no facts still produces tables: every file
    /// has none, which is not the same as having lost them.
    #[test]
    fn test_finish_default_module_facts_every_file_empty_not_missing() {
        let mut pool = StringPool::new();
        let files = vec![file(&mut pool, "a.py"), file(&mut pool, "b.py")];
        let graph = GraphAssembly {
            string_pool: pool,
            files,
            nodes: (0..9).map(|_| Node::default()).collect(),
            ..Default::default()
        }
        .finish();
        assert_eq!(graph.file_imports, [0, 0, 0]);
        assert_eq!(graph.file_flags, [0, 0]);
        assert_eq!(graph.exported_nodes.len(), 2, "9 nodes need 2 bitmap bytes");
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert!(archived.has_module_facts());
        assert_eq!(archived.file_imports(1).map(<[_]>::len), Some(0));

        let mut missing = graph;
        missing.file_imports.clear();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&missing).unwrap();
        let archived =
            rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert!(!archived.has_module_facts());
        assert!(
            archived.file_imports(0).is_none(),
            "missing must not read as empty"
        );
    }

    #[test]
    fn test_file_idx_of_unordered_and_ordered_files_finds_exact_paths_only() {
        for paths in [
            ["src/b.rs", "src/a.rs", "src/a/x.rs", "lib.rs"],
            // `Path` order, which the builder sorts by: `src/a/x.rs` sorts
            // before `src/a.rs`, unlike the bytes.
            ["lib.rs", "src/a/x.rs", "src/a.rs", "src/b.rs"],
        ] {
            let mut pool = StringPool::new();
            let files = paths.iter().map(|path| file(&mut pool, path)).collect();
            let graph = GraphAssembly {
                string_pool: pool,
                files,
                ..Default::default()
            }
            .finish();
            assert_eq!(
                graph.file_path_order.is_empty(),
                paths[0] == "lib.rs",
                "{paths:?}: a builder-ordered list needs no permutation"
            );
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
            let archived =
                rkyv::access::<crate::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
                    .unwrap();
            for (idx, path) in paths.iter().enumerate() {
                assert_eq!(archived.file_idx_of(path), Some(idx as u32), "{path}");
            }
            assert_eq!(archived.file_idx_of("src/a//x.rs"), None);
            assert_eq!(archived.file_idx_of("src/c.rs"), None);
            assert_eq!(archived.file_idx_of(""), None);
        }
    }

    #[test]
    #[should_panic(expected = "no CSR table")]
    fn test_finish_imports_without_offsets_panics() {
        let mut pool = StringPool::new();
        let files = vec![file(&mut pool, "a.py")];
        let source = pool.add("m");
        GraphAssembly {
            string_pool: pool,
            files,
            module_facts: ModuleFactTables {
                imports: vec![ImportFact {
                    source,
                    imported_name: source,
                    alias: None,
                    binding_kind: None,
                    target_file: None,
                }],
                ..ModuleFactTables::default()
            },
            ..Default::default()
        }
        .finish();
    }

    #[test]
    fn test_finish_supertype_table_sorted_by_type_keeping_first_of_a_repeat() {
        let fact = |type_idx, base| SupertypeFact {
            type_idx,
            bases: vec![base],
            first_unresolved: None,
        };
        let g = GraphAssembly {
            supertype_table: vec![fact(5, 1), fact(2, 9), fact(5, 7)],
            ..Default::default()
        }
        .finish();
        let got: Vec<(u32, Vec<u32>)> = g
            .supertype_table
            .iter()
            .map(|f| (f.type_idx, f.bases.clone()))
            .collect();
        assert_eq!(got, vec![(2, vec![9]), (5, vec![1])]);
    }

    #[test]
    fn test_finish_lexical_parents_sorted_by_node_keeping_first_of_a_repeat() {
        let row = |node_idx, parent_idx| LexicalParentFact {
            node_idx,
            parent_idx,
        };
        let g = GraphAssembly {
            lexical_parents: vec![row(5, 1), row(2, 9), row(5, 7)],
            ..Default::default()
        }
        .finish();
        assert_eq!(g.lexical_parents, vec![row(2, 9), row(5, 1)]);
    }

    #[test]
    fn test_finish_alias_table_kept_in_given_order() {
        let mut pool = StringPool::new();
        let facts: Vec<PathAliasFact> = ["b/*", "a/*", "b/*"]
            .into_iter()
            .map(|pattern| PathAliasFact {
                pattern: pool.add(pattern),
                replacements: vec![pool.add("src/*")],
            })
            .collect();
        let g = GraphAssembly {
            string_pool: pool,
            alias_table: facts.clone(),
            ..Default::default()
        }
        .finish();
        assert_eq!(g.alias_table, facts);
    }
}
