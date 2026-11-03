//! `HasMethod` / `HasProperty` edge emission — Class membership derivation.
//!
//! The derivation itself (the span pass, the parser-direct `owner_class`
//! pass, emission convention B.1) lives in
//! [`ecp_core::analyzer::membership`], shared with the overlay's per-file
//! replay. This pass maps each raw row to its node through Pass 1's
//! `RawIndexMap`, and answers the owner pass's name lookup from the
//! [`SymbolTable`].

use crate::resolution::builder::{skipped_nodes, SharedPaths};
use crate::resolution::index::SymbolTable;
use ecp_core::analyzer::lexical::forward_slash_path;
use ecp_core::analyzer::membership::{
    self, MemberLookup, MemberRow, CLASS_MEMBERSHIP_IMPL_REASON, CLASS_MEMBERSHIP_REASON,
};
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::graph::Edge;
use ecp_core::pool::StringPool;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

/// Emit `HasMethod` / `HasProperty` edges by walking the raw per-file
/// `LocalGraph`s, each end the node of its own raw row.
/// Returns the number of edges appended to `edges_out`.
///
/// Idempotent w.r.t. the input — re-running on the same `local_graphs` would
/// emit the same edge set. Callers must ensure they don't double-invoke.
///
/// `symbol_skip_set` — T7-6 incremental skip data. When `Some`, files whose
/// ALL symbols are in the skip set bypass both pass-1 span and pass-2 impl
/// emission. `None` = full reanalyze for all files (default, full-build path).
/// A path of `shared` skips nothing ([`skipped_nodes`]).
///
/// `raw_map` is Pass 1's map over `local_graphs`, in the same order: every
/// raw row maps to its node, a merged row to its survivor.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
    symbol_skip_set: Option<&FxHashMap<String, FxHashSet<u64>>>,
    shared: &SharedPaths,
) -> usize {
    let reason = string_pool.add(CLASS_MEMBERSHIP_REASON);
    let reason_impl = string_pool.add(CLASS_MEMBERSHIP_IMPL_REASON);

    // Per-file work is independent: writes go to a thread-local edge buffer
    // and only `symbol_table` (read-only) is shared. Same parallel pattern as
    // post_process::imports_edges.
    let chunk_results: Vec<Vec<Edge>> = local_graphs
        .par_iter()
        .enumerate()
        .map(|(file_ord, local_graph)| {
            // T7-6: skip class_membership for files whose every symbol is
            // unchanged, by Pass 1's uids (overloads numbered).
            if skipped_nodes(local_graph, file_ord as u32, symbol_skip_set, shared)
                .is_some_and(|skipped| skipped.iter().all(|&s| s))
            {
                return Vec::new();
            }
            let raws = &local_graph.nodes;
            let path_str_cow = forward_slash_path(&local_graph.file_path);
            let path_str: &str = &path_str_cow;

            let file = FileOrd(file_ord as u32);
            let mut local_edges: Vec<Edge> = Vec::new();
            membership::class_membership(
                raws,
                member_row,
                || IndexLookup {
                    symbol_table,
                    path: path_str,
                    raw_map,
                    file,
                    merged: raw_map.has_merged(file),
                },
                |m| {
                    local_edges.push(Edge {
                        source: m.source,
                        target: m.target,
                        rel_type: m.rel_type,
                        confidence: 1.0,
                        reason: match m.reason {
                            CLASS_MEMBERSHIP_REASON => reason,
                            _ => reason_impl,
                        },
                    })
                },
            );
            local_edges
        })
        .collect();

    let mut emitted = 0usize;
    for mut local_edges in chunk_results {
        emitted += local_edges.len();
        edges_out.append(&mut local_edges);
    }
    emitted
}

fn member_row(n: &RawNode) -> MemberRow<'_> {
    MemberRow {
        kind: n.kind,
        owner: n.owner_class.as_deref(),
        name: &n.name,
        span: n.span,
    }
}

/// The index's answers to membership's lookups over one file's path.
struct IndexLookup<'a> {
    symbol_table: &'a SymbolTable,
    path: &'a str,
    raw_map: &'a RawIndexMap,
    file: FileOrd,
    merged: bool,
}

impl MemberLookup for IndexLookup<'_> {
    fn lookup_in_file(&self, name: &str) -> Option<u32> {
        self.symbol_table.lookup_in_file(self.path, name)
    }

    fn node(&self, raw: usize) -> Option<u32> {
        self.raw_map.node(self.file, RawIdx(raw as u32))
    }

    fn has_merged(&self) -> bool {
        self.merged
    }

    fn is_scoped(&self, raw: usize) -> bool {
        self.node(raw)
            .is_some_and(|id| self.symbol_table.is_lexically_scoped(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::lexical::{file_uids, raw_decl};
    use ecp_core::analyzer::types::LocalGraph;
    use ecp_core::graph::{NodeKind, RelType};
    use std::path::PathBuf;

    fn raw(name: &str, kind: NodeKind, span: (u32, u32, u32, u32)) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        }
    }

    fn raw_with_owner_class(
        name: &str,
        kind: NodeKind,
        span: (u32, u32, u32, u32),
        owner_class: &str,
    ) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: Some(owner_class.to_string()),
            content_hash: 0,
        }
    }

    fn build_setup(local_graphs: Vec<LocalGraph>) -> (SymbolTable, StringPool, Vec<Edge>) {
        let mut symbol_table = SymbolTable::new();
        let string_pool = StringPool::new();
        let mut current = 0u32;
        for lg in &local_graphs {
            let path_str = lg.file_path.to_string_lossy().replace('\\', "/");
            for rn in &lg.nodes {
                symbol_table.register_node(&path_str, &rn.name, current, rn.kind);
                current += 1;
            }
        }
        (symbol_table, string_pool, Vec::new())
    }

    fn run(local_graphs: Vec<LocalGraph>) -> Vec<Edge> {
        run_with(local_graphs, Vec::new(), None)
    }

    /// `merged` holds Pass 1's `(tombstone, survivor)` slots.
    fn run_with(
        local_graphs: Vec<LocalGraph>,
        merged: Vec<(u32, u32)>,
        skip: Option<&FxHashMap<String, FxHashSet<u64>>>,
    ) -> Vec<Edge> {
        let (symbol_table, mut string_pool, mut edges) = build_setup(local_graphs.clone());
        let raw_map = RawIndexMap::new(0, local_graphs.iter().map(|lg| lg.nodes.len()))
            .unwrap()
            .with_merged(merged)
            .unwrap();
        emit_edges(
            &local_graphs,
            &raw_map,
            &symbol_table,
            &mut string_pool,
            &mut edges,
            skip,
            &SharedPaths::of(&local_graphs),
        );
        edges
    }

    fn pairs(edges: &[Edge]) -> Vec<(u32, u32)> {
        edges.iter().map(|e| (e.source, e.target)).collect()
    }

    fn lg(path: &str, nodes: Vec<RawNode>) -> LocalGraph {
        LocalGraph {
            file_path: PathBuf::from(path),
            content_hash: [0u8; 8],
            nodes,
            documents: Vec::new(),
            imports: Vec::new(),
            routes: Vec::new(),
            route_receivers: None,
            framework_refs: Vec::new(),
            fanout_refs: Vec::new(),
            blind_spots: Vec::new(),
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: Vec::new(),
            raw_function_metas: vec![],
            export_aliases: None,
            parsed_language: ecp_core::contracts::routing::ParsedLanguage::ABSENT,
        }
    }

    #[test]
    fn single_class_emits_methods() {
        // class Foo (1..10): m1 (2..3), m2 (4..5), m3 (6..7)
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 10, 0)),
            raw("m1", NodeKind::Method, (2, 4, 3, 4)),
            raw("m2", NodeKind::Method, (4, 4, 5, 4)),
            raw("m3", NodeKind::Method, (6, 4, 7, 4)),
        ];
        let edges = run(vec![lg("foo.ts", nodes)]);
        assert_eq!(edges.len(), 3);
        for e in &edges {
            assert!(matches!(e.rel_type, RelType::HasMethod));
            assert_eq!(e.source, 0); // Foo
        }
    }

    #[test]
    fn nested_class_attributes_methods_to_innermost() {
        // class Outer (1..20):
        //   def outer_m (2..3)              → HasMethod(Outer)
        //   class Inner (5..15):
        //     def inner_m (6..7)            → HasMethod(Inner), NOT Outer
        let nodes = vec![
            raw("Outer", NodeKind::Class, (1, 0, 20, 0)),
            raw("outer_m", NodeKind::Method, (2, 4, 3, 4)),
            raw("Inner", NodeKind::Class, (5, 4, 15, 4)),
            raw("inner_m", NodeKind::Method, (6, 8, 7, 8)),
        ];
        let edges = run(vec![lg("nested.py", nodes)]);
        assert_eq!(edges.len(), 2);
        let outer_m_edges: Vec<_> = edges.iter().filter(|e| e.target == 1).collect();
        let inner_m_edges: Vec<_> = edges.iter().filter(|e| e.target == 3).collect();
        assert_eq!(outer_m_edges.len(), 1);
        assert_eq!(outer_m_edges[0].source, 0); // Outer
        assert_eq!(inner_m_edges.len(), 1);
        assert_eq!(inner_m_edges[0].source, 2); // Inner
    }

    #[test]
    fn top_level_function_not_attributed() {
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 5, 0)),
            raw("m", NodeKind::Method, (2, 4, 3, 4)),
            raw("free_fn", NodeKind::Function, (10, 0, 12, 0)),
        ];
        let edges = run(vec![lg("toplevel.ts", nodes)]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, 1); // m only
    }

    #[test]
    fn class_with_only_properties() {
        let nodes = vec![
            raw("Bag", NodeKind::Class, (1, 0, 5, 0)),
            raw("x", NodeKind::Property, (2, 4, 2, 10)),
            raw("y", NodeKind::Property, (3, 4, 3, 10)),
        ];
        let edges = run(vec![lg("bag.ts", nodes)]);
        assert_eq!(edges.len(), 2);
        assert!(edges
            .iter()
            .all(|e| matches!(e.rel_type, RelType::HasProperty)));
    }

    #[test]
    fn empty_class_no_edges() {
        let nodes = vec![raw("Empty", NodeKind::Class, (1, 0, 2, 0))];
        let edges = run(vec![lg("empty.ts", nodes)]);
        assert_eq!(edges.len(), 0);
    }

    #[test]
    fn multi_class_same_file_disjoint_spans() {
        // class A (1..5): a_m
        // class B (10..15): b_m
        let nodes = vec![
            raw("A", NodeKind::Class, (1, 0, 5, 0)),
            raw("a_m", NodeKind::Method, (2, 4, 3, 4)),
            raw("B", NodeKind::Class, (10, 0, 15, 0)),
            raw("b_m", NodeKind::Method, (11, 4, 12, 4)),
        ];
        let edges = run(vec![lg("ab.ts", nodes)]);
        assert_eq!(edges.len(), 2);
        let a_edge = edges.iter().find(|e| e.target == 1).unwrap();
        let b_edge = edges.iter().find(|e| e.target == 3).unwrap();
        assert_eq!(a_edge.source, 0); // A
        assert_eq!(b_edge.source, 2); // B
    }

    #[test]
    fn rust_inherent_impl_via_owner_class() {
        // struct Foo (1..1) — single line decl
        // fn new (5..7) owner_class="Foo", kind=Function
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 1, 12)),
            raw_with_owner_class("new", NodeKind::Function, (5, 4, 7, 4), "Foo"),
        ];
        let edges = run(vec![lg("foo.rs", nodes)]);
        assert_eq!(edges.len(), 1);
        assert!(matches!(edges[0].rel_type, RelType::HasMethod));
        assert_eq!(edges[0].source, 0); // Foo
        assert_eq!(edges[0].target, 1); // new
    }

    #[test]
    fn rust_trait_impl_via_owner_class() {
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 1, 12)),
            raw_with_owner_class("fmt", NodeKind::Method, (5, 4, 7, 4), "Foo"),
        ];
        let edges = run(vec![lg("foo.rs", nodes)]);
        assert_eq!(edges.len(), 1);
        assert!(matches!(edges[0].rel_type, RelType::HasMethod));
        assert_eq!(edges[0].source, 0); // Foo
        assert_eq!(edges[0].target, 1); // fmt
    }

    #[test]
    fn python_def_as_function_still_emits() {
        // Python parser emits class-body `def` as kind=Function (not Method).
        // B.1: HasMethod edge still emitted regardless of target kind.
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 5, 0)),
            raw("bar", NodeKind::Function, (2, 4, 3, 4)),
        ];
        let edges = run(vec![lg("foo.py", nodes)]);
        assert_eq!(edges.len(), 1);
        assert!(matches!(edges[0].rel_type, RelType::HasMethod));
        assert_eq!(edges[0].target, 1); // bar
    }

    #[test]
    fn java_style_same_named_constructor_gets_has_method_without_self_loop() {
        // Java emits `class Foo { public Foo() {} }` as Class "Foo" containing
        // Method "Foo". lookup_in_file returns the class for both names; the
        // member binds its own row.
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 10, 0)),
            raw("Foo", NodeKind::Method, (2, 4, 4, 4)),
        ];
        let edges = run(vec![lg("Foo.java", nodes)]);
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert!(matches!(edges[0].rel_type, RelType::HasMethod));
        assert_eq!(edges[0].target, 1, "constructor, not the class itself");
    }

    #[test]
    fn rust_pass2_skips_when_pass1_covered() {
        // If a Class's span DOES contain the method's span AND owner_class is set,
        // Pass 1 emits and Pass 2 must skip (no duplicate).
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 10, 0)),
            raw_with_owner_class("bar", NodeKind::Method, (3, 4, 5, 4), "Foo"),
        ];
        let edges = run(vec![lg("foo.rs", nodes)]);
        assert_eq!(edges.len(), 1, "must not double-emit");
    }

    /// Stands for: membership binds by row (design_uid §3 row 5). Python
    /// `class C` twice, `a` in the first body and `b` in the second:
    /// `HasMethod C@1 -> a`, `C@2 -> b`, nothing else.
    #[test]
    fn test_emit_edges_class_declared_twice_each_owns_its_own_body() {
        let nodes = vec![
            raw("C", NodeKind::Class, (1, 0, 3, 0)),
            raw("a", NodeKind::Function, (2, 4, 2, 9)),
            raw("C", NodeKind::Class, (5, 0, 7, 0)),
            raw("b", NodeKind::Function, (6, 4, 6, 9)),
        ];
        let edges = run(vec![lg("c.py", nodes)]);
        assert_eq!(pairs(&edges), [(0, 1), (2, 3)]);
    }

    /// Two merged `x` rows of one class give one `HasProperty`, to the
    /// survivor.
    #[test]
    fn test_emit_edges_merged_property_rows_emit_one_has_property() {
        let nodes = vec![
            raw("K", NodeKind::Class, (1, 0, 9, 0)),
            raw("x", NodeKind::Property, (2, 4, 2, 9)),
            raw("x", NodeKind::Property, (5, 4, 5, 9)),
        ];
        let edges = run_with(vec![lg("k.ts", nodes)], vec![(2, 1)], None);
        assert_eq!(pairs(&edges), [(0, 1)]);
        assert!(matches!(edges[0].rel_type, RelType::HasProperty));
    }

    /// T7-6: a path with one input whose every uid is unchanged is skipped;
    /// a path with two inputs skips nothing, as Pass 1 numbers it as one
    /// file and `file_uids` each input alone.
    #[test]
    fn test_emit_edges_skip_set_two_inputs_one_path_skips_nothing() {
        let one = vec![
            raw("K", NodeKind::Class, (1, 0, 9, 0)),
            raw("m", NodeKind::Method, (2, 4, 3, 4)),
        ];
        let skip_of = |path: &str, inputs: &[&Vec<RawNode>]| {
            let uids: FxHashSet<u64> = inputs
                .iter()
                .flat_map(|nodes| file_uids(path, nodes.as_slice(), raw_decl))
                .collect();
            FxHashMap::from_iter([(path.to_string(), uids)])
        };

        let single = skip_of("k.ts", &[&one]);
        assert!(run_with(vec![lg("k.ts", one.clone())], Vec::new(), Some(&single)).is_empty());

        let two = skip_of("k.vue", &[&one, &one]);
        let edges = run_with(
            vec![lg("k.vue", one.clone()), lg("k.vue", one.clone())],
            Vec::new(),
            Some(&two),
        );
        assert_eq!(pairs(&edges), [(0, 1), (2, 3)]);
    }
}
