//! Synthetic graphs for tests, declared as symbols and edges.
//!
//! A hand-written [`ZeroCopyGraph`] literal makes every test know the
//! storage layout — 22 fields, four index arrays that must agree with the
//! edge list, uid hashing, and string interning — to say something as small
//! as "`caller` calls `callee`". That knowledge belongs to
//! [`GraphAssembly`](crate::graph_assembly::GraphAssembly), which the real
//! builder already uses; this is the same thing with the ceremony removed:
//!
//! ```
//! use ecp_core::graph::RelType;
//! use ecp_core::graph_fixture::GraphFixture;
//!
//! let mut fx = GraphFixture::new();
//! let caller = fx.func("src/x.ts", "caller");
//! let callee = fx.func("src/x.ts", "callee");
//! fx.edge(caller, callee, RelType::Calls);
//! let bytes = fx.into_bytes();
//! ```
//!
//! Fixtures built this way carry the derived indices (`name_index`,
//! `kind_offsets`, both CSR directions) that a real graph has, so a test
//! exercises the same lookup paths production does rather than the
//! empty-index fallbacks.

use crate::graph::{
    BlindSpotRecord, CallMeta, Edge, File, FileCategory, FunctionMeta, Node, NodeKind, RelType,
    RouteShape, ZeroCopyGraph,
};
use crate::graph_assembly::GraphAssembly;
use crate::pool::StrRef;
use rustc_hash::FxHashMap;

/// Declarative builder for a synthetic graph. Node and edge adders return
/// the index they appended at; everything derived is derived at
/// [`build`](Self::build).
#[derive(Default)]
pub struct GraphFixture {
    asm: GraphAssembly,
    file_idx: FxHashMap<String, u32>,
    processes: u32,
}

impl GraphFixture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `path` as a `Source` file, or return its existing index.
    pub fn file(&mut self, path: &str) -> u32 {
        self.file_as(path, FileCategory::Source)
    }

    /// Register `path` with an explicit category. The category of an
    /// already-registered path is left alone.
    pub fn file_as(&mut self, path: &str, category: FileCategory) -> u32 {
        if let Some(&idx) = self.file_idx.get(path) {
            return idx;
        }
        let idx = self.asm.files.len() as u32;
        let path_ref = self.asm.string_pool.add(path);
        self.asm.files.push(File {
            path: path_ref,
            mtime: 0,
            content_hash: [0u8; 8],
            category,
        });
        self.file_idx.insert(path.to_string(), idx);
        idx
    }

    /// Append a node, registering its file if needed. Span defaults to all
    /// zeroes — set one with [`span`](Self::span) when the test asserts on
    /// line numbers.
    pub fn node(&mut self, kind: NodeKind, path: &str, name: &str) -> u32 {
        self.push_node(kind, path, None, name)
    }

    /// Append a `Function` node.
    pub fn func(&mut self, path: &str, name: &str) -> u32 {
        self.push_node(NodeKind::Function, path, None, name)
    }

    /// Append a `Method` node owned by `owner_class` — the owner is part of
    /// the uid, so `Foo.validate` and `Bar.validate` stay distinct.
    pub fn method(&mut self, path: &str, owner_class: &str, name: &str) -> u32 {
        self.node_owned(NodeKind::Method, path, owner_class, name)
    }

    /// Append a node of any kind owned by `owner_class` — `SchemaField`,
    /// `Property`, and `Constructor` carry an owner the same way `Method`
    /// does, and it must reach the uid, not just the field.
    pub fn node_owned(&mut self, kind: NodeKind, path: &str, owner_class: &str, name: &str) -> u32 {
        self.push_node(kind, path, Some(owner_class), name)
    }

    /// Append a `Process` node and its trace. Processes must be appended
    /// contiguously: `process_start` marks one block, not a scattering.
    pub fn process(&mut self, path: &str, name: &str, trace: &[u32]) -> u32 {
        let idx = self.push_node(NodeKind::Process, path, None, name);
        if self.processes == 0 {
            self.asm.process_start = idx;
        } else {
            assert_eq!(
                self.asm.process_start + self.processes,
                idx,
                "Process nodes must be contiguous — {} non-Process node(s) were appended \
                 between them, which breaks the process_start/traces_offsets pairing",
                idx - (self.asm.process_start + self.processes)
            );
        }
        self.processes += 1;
        self.asm.traces_data.extend_from_slice(trace);
        self.asm
            .traces_offsets
            .push(self.asm.traces_data.len() as u32);
        idx
    }

    fn push_node(
        &mut self,
        kind: NodeKind,
        path: &str,
        owner_class: Option<&str>,
        name: &str,
    ) -> u32 {
        let file_idx = self.file(path);
        let uid = crate::uid::compute(kind, path, owner_class, name);
        let name_ref = self.asm.string_pool.add(name);
        let owner_ref = owner_class
            .map(|o| self.asm.string_pool.add(o))
            .unwrap_or_default();
        let idx = self.asm.nodes.len() as u32;
        self.asm.nodes.push(Node {
            uid,
            name: name_ref,
            file_idx,
            kind,
            span: (0, 0, 0, 0),
            community_id: 0,
            owner_class: owner_ref,
            content_hash: 0,
        });
        idx
    }

    /// Append a fully-trusted edge (`confidence` 1.0, reason `"ast-call"`).
    /// Returns the edge index to pass to [`call_meta`](Self::call_meta).
    pub fn edge(&mut self, source: u32, target: u32, rel: RelType) -> u32 {
        self.edge_with(source, target, rel, 1.0, "ast-call")
    }

    /// Append an edge with an explicit confidence and reason — the pair
    /// impact's heuristic filtering reads.
    pub fn edge_with(
        &mut self,
        source: u32,
        target: u32,
        rel: RelType,
        confidence: f32,
        reason: &str,
    ) -> u32 {
        let reason = self.asm.string_pool.add(reason);
        let idx = self.asm.edges.len() as u32;
        self.asm.edges.push(Edge {
            source,
            target,
            rel_type: rel,
            confidence,
            reason,
        });
        idx
    }

    /// Attach dispatch metadata to an edge, addressed by the index
    /// [`edge`](Self::edge) returned.
    pub fn call_meta(&mut self, edge: u32, flags: u8, dispatch_type: &str) {
        let dispatch_type = self.asm.string_pool.add(dispatch_type);
        self.asm.call_metas.push(CallMeta {
            edge_idx: edge,
            flags,
            dispatch_type,
        });
    }

    /// Attach signature metadata to a node. `node_flags` is derived from it.
    /// `params` / `return_type` have no helper — reach for
    /// [`assembly_mut`](Self::assembly_mut) when a test needs them.
    pub fn function_meta(&mut self, node: u32, flags: u16, decorators: &[&str]) {
        let decorators = decorators
            .iter()
            .map(|d| self.asm.string_pool.add(d))
            .collect();
        self.asm.function_metas.push(FunctionMeta {
            node_idx: node,
            flags,
            params: Vec::new(),
            return_type: StrRef::default(),
            decorators,
        });
    }

    /// Record an unresolvable-pattern site.
    pub fn blind_spot(&mut self, kind: &str, path: &str, span: (u32, u32, u32, u32), hint: &str) {
        let kind = self.asm.string_pool.add(kind);
        let file_path = self.asm.string_pool.add(path);
        let hint = self.asm.string_pool.add(hint);
        self.asm.blind_spots.push(BlindSpotRecord {
            kind,
            file_path,
            start_row: span.0,
            start_col: span.1,
            end_row: span.2,
            end_col: span.3,
            hint,
            is_test: false,
        });
    }

    /// Record a Route's response shape, as `shape_check` reads it.
    pub fn route_shape(&mut self, node: u32, response_keys: &[&str], error_keys: &[&str]) {
        let response_keys = response_keys
            .iter()
            .map(|k| self.asm.string_pool.add(k))
            .collect();
        let error_keys = error_keys
            .iter()
            .map(|k| self.asm.string_pool.add(k))
            .collect();
        self.asm.route_shapes.push(RouteShape {
            node_idx: node,
            response_keys,
            error_keys,
        });
    }

    /// Set a node's source span.
    pub fn span(&mut self, node: u32, span: (u32, u32, u32, u32)) {
        self.asm.nodes[node as usize].span = span;
    }

    /// Put a node in a community — what the Leiden pass assigns, and what
    /// `processes` partitions traces by.
    pub fn community(&mut self, node: u32, community_id: u16) {
        self.asm.nodes[node as usize].community_id = community_id;
    }

    /// Intern a string, for the raw records the helpers above don't build.
    pub fn intern(&mut self, s: &str) -> StrRef {
        self.asm.string_pool.add(s)
    }

    /// The `GraphAssembly` under construction, for a field with no helper.
    pub fn assembly_mut(&mut self) -> &mut GraphAssembly {
        &mut self.asm
    }

    pub fn build(mut self) -> ZeroCopyGraph {
        if self.processes == 0 {
            // Mirrors the real builder: with no Process block, the boundary
            // sits past the last node rather than at 0.
            self.asm.process_start = self.asm.nodes.len() as u32;
        }
        self.asm.finish()
    }

    /// The archived bytes, ready for `rkyv::access` or a `graph.bin` write.
    pub fn into_bytes(self) -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(&self.build())
            .expect("fixture graph serialization")
            .to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ArchivedZeroCopyGraph;

    #[test]
    fn fixture_edges_are_reachable_through_the_out_csr() {
        let mut fx = GraphFixture::new();
        let caller = fx.func("src/x.ts", "caller");
        let callee = fx.func("src/x.ts", "callee");
        fx.edge(caller, callee, RelType::Calls);
        let g = fx.build();

        let start = g.out_offsets[caller as usize] as usize;
        let end = g.out_offsets[caller as usize + 1] as usize;
        assert_eq!(end - start, 1);
        assert_eq!(g.edges[start].target, callee);
        assert_eq!(g.files.len(), 1, "same path must reuse one file entry");
    }

    #[test]
    fn fixture_nodes_are_findable_by_name_and_kind() {
        let mut fx = GraphFixture::new();
        fx.func("a.rs", "solo");
        fx.method("a.rs", "Foo", "shared");
        fx.method("a.rs", "Bar", "shared");
        let bytes = fx.into_bytes();
        let g = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();

        assert_eq!(g.nodes_by_name("shared").count(), 2);
        assert_eq!(g.nodes_by_name("solo").collect::<Vec<_>>(), vec![0]);
        assert_eq!(g.nodes_by_kind(NodeKind::Method).count(), 2);
    }

    #[test]
    fn fixture_owner_class_separates_same_named_methods() {
        let mut fx = GraphFixture::new();
        let foo = fx.method("a.rs", "Foo", "validate");
        let bar = fx.method("a.rs", "Bar", "validate");
        let g = fx.build();
        assert_ne!(g.nodes[foo as usize].uid, g.nodes[bar as usize].uid);
    }

    #[test]
    fn fixture_call_meta_survives_the_edge_sort() {
        let mut fx = GraphFixture::new();
        let a = fx.func("a.rs", "a");
        let b = fx.func("a.rs", "b");
        // Appended out of source order, so the sort moves it.
        let late = fx.edge(b, a, RelType::Calls);
        fx.edge(a, b, RelType::Calls);
        fx.call_meta(late, CallMeta::FLAG_CALLBACK, "FnPtr");
        let g = fx.build();

        let meta = g.call_metas.first().expect("call meta kept");
        assert!(meta.is_callback());
        assert_eq!(g.edges[meta.edge_idx as usize].source, b);
    }

    #[test]
    fn fixture_process_traces_line_up_with_process_start() {
        let mut fx = GraphFixture::new();
        let a = fx.func("a.rs", "a");
        let b = fx.func("a.rs", "b");
        let p0 = fx.process("a.rs", "Flow A", &[a, b]);
        fx.process("a.rs", "Flow B", &[b]);
        let g = fx.build();

        assert_eq!(g.process_start, p0);
        assert_eq!(g.traces_offsets, vec![0, 2, 3]);
        let k = 1usize; // second process
        let start = g.traces_offsets[k] as usize;
        let end = g.traces_offsets[k + 1] as usize;
        assert_eq!(&g.traces_data[start..end], &[b]);
    }

    #[test]
    #[should_panic(expected = "Process nodes must be contiguous")]
    fn fixture_rejects_a_node_wedged_between_processes() {
        let mut fx = GraphFixture::new();
        fx.process("a.rs", "Flow A", &[]);
        fx.func("a.rs", "interloper");
        fx.process("a.rs", "Flow B", &[]);
    }
}
