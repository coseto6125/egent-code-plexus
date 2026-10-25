//! `HasMethod` / `HasProperty` edge emission — Class membership derivation.
//!
//! Two-pass strategy (see spec `docs/superpowers/specs/2026-05-16-class-membership-postprocess.md`):
//!
//! - **Pass 1** — span containment, covers 30 languages whose class body
//!   syntactically encloses its methods/properties (TS / Ruby / Java / PHP /
//!   Dart / Swift / C++ / C# / Python / etc.).
//! - **Pass 2** — parser-direct `owner_class` bridge. All parsers (since T1-1)
//!   stamp each method's owning type directly into `RawNode.owner_class`. This
//!   pass reads that field and emits the matching `HasMethod` edge.
//!
//! Emission convention is **B.1** (5-agent design review): a single edge
//! type `HasMethod` regardless of target kind (`Method` or `Function`). This
//! lets the cypher query `MATCH (c:Class)-[:HasMethod]->(m) RETURN m` work
//! cross-language without callers having to learn per-language target-kind
//! routing rules.

use crate::framework_helpers::{span_area, span_contains, Span};
use crate::resolution::index::SymbolTable;
use ecp_core::analyzer::lexical::{file_uids, raw_decl};
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{Edge, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;

/// Emit `HasMethod` / `HasProperty` edges by walking the raw per-file
/// `LocalGraph`s and resolving names back to node indices via `SymbolTable`.
/// Returns the number of edges appended to `edges_out`.
///
/// Idempotent w.r.t. the input — re-running on the same `local_graphs` would
/// emit the same edge set. Callers must ensure they don't double-invoke.
///
/// `symbol_skip_set` — T7-6 incremental skip data. When `Some`, files whose
/// ALL symbols are in the skip set bypass both pass-1 span and pass-2 impl
/// emission. `None` = full reanalyze for all files (default, full-build path).
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
    symbol_skip_set: Option<&FxHashMap<String, FxHashSet<u64>>>,
) -> usize {
    let reason = string_pool.add("post_process:class_membership");
    let reason_impl = string_pool.add("post_process:class_membership:rust_impl");

    // Per-file work is independent: writes go to a thread-local edge buffer
    // and only `symbol_table` (read-only) is shared. Same parallel pattern as
    // post_process::imports_edges.
    let chunk_results: Vec<(usize, Vec<Edge>)> = local_graphs
        .par_iter()
        .map(|local_graph| {
            let raws = &local_graph.nodes;

            // Forward-slash normalize once. Linux/macOS paths have no `\\`
            // → reuse the borrowed cow without allocating.
            let raw_path = local_graph.file_path.to_string_lossy();
            let path_str_cow: Cow<'_, str> = if raw_path.contains('\\') {
                Cow::Owned(raw_path.replace('\\', "/"))
            } else {
                raw_path
            };
            let path_str: &str = &path_str_cow;

            // T7-6: skip class_membership for files whose every symbol is
            // unchanged, by Pass 1's uids (overloads numbered).
            if let Some(skip_uids) = symbol_skip_set.and_then(|m| m.get(path_str)) {
                if file_uids(path_str, raws, raw_decl)
                    .iter()
                    .all(|uid| skip_uids.contains(uid))
                {
                    return (0, Vec::new());
                }
            }

            // Pre-pass: collect Class-like nodes + group by name. Both passes hot-loop
            // over this O(N) once-per-file index instead of re-scanning `raws`
            // for kind=Class on every member lookup. Typical K = #classes/file
            // is 1-10, so this drops both passes from O(N²) to O(N·K) and lets
            // class-free files short-circuit cleanly.
            // Includes Struct/Trait/Interface so Rust `struct Foo` + `impl Foo`
            // bridge correctly now that structs no longer emit as Class.
            // An enum's methods and cases have separate semantics, but a
            // `Const` inside it is a type member just like a class constant.
            // Retain the kind so only constants use enum owners, and the
            // generic arity so C# `Conv<T>` owns its own members, not `Conv`.
            let mut owners: Vec<Owner<'_>> = Vec::new();
            let mut classes_by_name: FxHashMap<&str, Vec<Span>> = FxHashMap::default();
            for n in raws {
                if matches!(
                    n.kind,
                    NodeKind::Class | NodeKind::Struct | NodeKind::Trait | NodeKind::Interface
                ) {
                    owners.push((n.name.as_str(), n.kind, n.span, n.generic_arity()));
                    classes_by_name
                        .entry(n.name.as_str())
                        .or_default()
                        .push(n.span);
                } else if n.kind == NodeKind::Enum {
                    owners.push((n.name.as_str(), n.kind, n.span, 0));
                }
            }
            if owners.is_empty() {
                return (0, Vec::new());
            }

            let later = later_overload_ids(raws, path_str, symbol_table);
            let mut local_edges: Vec<Edge> = Vec::new();
            let mut local_emitted = 0usize;
            local_emitted += emit_pass1_span(
                raws,
                &owners,
                path_str,
                symbol_table,
                &later,
                reason,
                &mut local_edges,
            );
            local_emitted += emit_pass2_rust_impl(
                raws,
                &classes_by_name,
                path_str,
                symbol_table,
                &later,
                reason_impl,
                &mut local_edges,
            );
            (local_emitted, local_edges)
        })
        .collect();

    let mut emitted = 0usize;
    for (count, mut local_edges) in chunk_results {
        emitted += count;
        edges_out.append(&mut local_edges);
    }
    emitted
}

/// A type that can own members: name, kind, span and declared generic arity
/// (`RawNode::generic_arity`).
type Owner<'a> = (&'a str, NodeKind, Span, u32);

/// Raw index → node id of each later overload in the file
/// ([`ecp_core::uid::keeps_overloads`]). A name lookup returns the first
/// declaration, so without this map every overload's edge would land on it.
fn later_overload_ids(
    raws: &[RawNode],
    path_str: &str,
    symbol_table: &SymbolTable,
) -> FxHashMap<usize, u32> {
    let mut seen: FxHashMap<(u8, Option<&str>, &str), usize> = FxHashMap::default();
    let mut ids = FxHashMap::default();
    for (i, raw) in raws.iter().enumerate() {
        if !ecp_core::uid::keeps_overloads(raw.kind) {
            continue;
        }
        let owner = raw.owner_class.as_deref();
        let ordinal = seen
            .entry((raw.kind as u8, owner, raw.name.as_str()))
            .or_insert(0);
        if *ordinal > 0 {
            if let Some(&id) = symbol_table
                .first_declaration(path_str, &raw.name, raw.kind, owner)
                .and_then(|first| symbol_table.later_overloads(first).get(*ordinal - 1))
            {
                ids.insert(i, id);
            }
        }
        *ordinal += 1;
    }
    ids
}

/// The node of `member` (raw index `i`) for an edge from `class_idx`, or
/// `None` to emit none: its own later overload, else the declaration its
/// kind and owner match, else the file's first declaration of the name.
/// Only that last lookup can return the class itself, for a member named
/// like it, which would be a self-loop.
fn member_node(
    i: usize,
    member: &RawNode,
    path_str: &str,
    symbol_table: &SymbolTable,
    later: &FxHashMap<usize, u32>,
    class_idx: u32,
) -> Option<u32> {
    let id = later
        .get(&i)
        .copied()
        .or_else(|| {
            symbol_table.first_declaration(
                path_str,
                &member.name,
                member.kind,
                member.owner_class.as_deref(),
            )
        })
        .or_else(|| symbol_table.lookup_in_file(path_str, &member.name))?;
    (id != class_idx).then_some(id)
}

/// Pass 1 — cross-language span containment.
///
/// For each Class node `c` in the file, find the innermost-enclosing Class for
/// every Function/Method/Property in the same file. If that innermost Class is
/// `c`, emit the corresponding `HasMethod` (Function|Method) or `HasProperty`
/// (Property) edge from `c` to the member.
fn emit_pass1_span(
    raws: &[RawNode],
    owners: &[Owner<'_>],
    path_str: &str,
    symbol_table: &SymbolTable,
    later: &FxHashMap<usize, u32>,
    reason: ecp_core::pool::StrRef,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let mut emitted = 0usize;

    for (i, member) in raws.iter().enumerate() {
        let rel_type = match member.kind {
            NodeKind::Function | NodeKind::Method | NodeKind::Constructor => RelType::HasMethod,
            NodeKind::Property => RelType::HasProperty,
            // A class constant (PHP `const STATUS`) carries its class as
            // owner, so `scope_defines` gives it no File edge.
            NodeKind::Const if member.owner_class.is_some() => RelType::HasProperty,
            _ => continue,
        };

        // Innermost enclosing class via smallest-area span. Scans the
        // pre-collected classes slice (K entries) instead of all raws.
        let Some((class_name, _, _, arity)) = owners
            .iter()
            .filter(|(_, kind, span, _)| {
                (member.kind == NodeKind::Const || *kind != NodeKind::Enum)
                    && span_contains(*span, member.span)
            })
            .min_by_key(|(_, _, span, _)| span_area(*span))
            .copied()
        else {
            continue;
        };

        // Namespace-qualified type identities are not represented by the
        // short-name lookup below. Do not bind an ambiguous type constant.
        if member.kind == NodeKind::Const
            && (member.owner_class.as_deref() != Some(class_name)
                || owners
                    .iter()
                    .filter(|(name, _, _, _)| *name == class_name)
                    .count()
                    != 1)
        {
            continue;
        }

        // The name lookup returns the first of same-named C# types of
        // different arity; the owner is the one of its own arity.
        let Some(class_idx) = symbol_table
            .lookup_in_file(path_str, class_name)
            .map(|first| symbol_table.with_arity(first, arity).unwrap_or(first))
        else {
            continue;
        };
        let Some(member_idx) = member_node(i, member, path_str, symbol_table, later, class_idx)
        else {
            continue;
        };

        edges_out.push(Edge {
            source: class_idx,
            target: member_idx,
            rel_type,
            confidence: 1.0,
            reason,
        });
        emitted += 1;
    }

    emitted
}

/// Pass 2 — parser-direct owner_class bridge.
///
/// Reads `RawNode.owner_class` set directly by each language parser
/// (Rust uses impl_map; Go uses recv_map; all others use span containment
/// within parse_file). Emits `HasMethod` / `HasProperty` for members whose
/// owning type's span does NOT contain the member's span — i.e., cases where
/// Pass 1 span containment would miss (Rust impl blocks, Go receiver methods).
///
/// Skip emission when Pass 1 already covered the link — checked by
/// span containment: if the member's span lies inside any Class span in
/// the same file, Pass 1 already emitted.
fn emit_pass2_rust_impl(
    raws: &[RawNode],
    classes_by_name: &FxHashMap<&str, Vec<Span>>,
    path_str: &str,
    symbol_table: &SymbolTable,
    later: &FxHashMap<usize, u32>,
    reason: ecp_core::pool::StrRef,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let mut emitted = 0usize;

    for (i, member) in raws.iter().enumerate() {
        if !matches!(
            member.kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constructor | NodeKind::Property
        ) {
            continue;
        }

        let Some(ref class_name) = member.owner_class else {
            continue;
        };

        // Skip if Pass 1 already emitted via span containment.
        let already_via_span = classes_by_name
            .get(class_name.as_str())
            .map(|spans| spans.iter().any(|s| span_contains(*s, member.span)))
            .unwrap_or(false);
        if already_via_span {
            continue;
        }

        let Some(class_idx) = symbol_table.lookup_in_file(path_str, class_name) else {
            continue;
        };
        let Some(member_idx) = member_node(i, member, path_str, symbol_table, later, class_idx)
        else {
            continue;
        };

        let rel_type = if matches!(member.kind, NodeKind::Property) {
            RelType::HasProperty
        } else {
            RelType::HasMethod
        };

        edges_out.push(Edge {
            source: class_idx,
            target: member_idx,
            rel_type,
            confidence: 1.0,
            reason,
        });
        emitted += 1;
    }

    emitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::types::LocalGraph;
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
        let (symbol_table, mut string_pool, mut edges) = build_setup(local_graphs.clone());
        emit_edges(
            &local_graphs,
            &symbol_table,
            &mut string_pool,
            &mut edges,
            None,
        );
        edges
    }

    fn lg(path: &str, nodes: Vec<RawNode>) -> LocalGraph {
        LocalGraph {
            file_path: PathBuf::from(path),
            content_hash: [0u8; 8],
            nodes,
            documents: Vec::new(),
            imports: Vec::new(),
            routes: Vec::new(),
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
        // Method "Foo". lookup_in_file returns the class for both names, so
        // member_node resolves the member through first_declaration (kind +
        // owner) and skips only a true self-loop.
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
}
