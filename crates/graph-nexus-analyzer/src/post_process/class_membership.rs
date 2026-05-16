//! `HasMethod` / `HasProperty` edge emission — Class membership derivation.
//!
//! Two-pass strategy (see spec `docs/superpowers/specs/2026-05-16-class-membership-postprocess.md`):
//!
//! - **Pass 1** — span containment, covers 30 languages whose class body
//!   syntactically encloses its methods/properties (TS / Ruby / Java / PHP /
//!   Dart / Swift / C++ / C# / Python / etc.).
//! - **Pass 2** — Rust `impl` bridge. Rust splits the struct/enum declaration
//!   and its `impl` blocks, so methods don't sit inside the Class span. The
//!   Rust parser stamps each impl method's owning type into `RawNode.heritage`
//!   as `__impl_target__:Foo`; this pass reads that sentinel and emits the
//!   matching `HasMethod` edge.
//!
//! Emission convention is **B.1** (5-agent design review): a single edge
//! type `HasMethod` regardless of target kind (`Method` or `Function`). This
//! lets the cypher query `MATCH (c:Class)-[:HasMethod]->(m) RETURN m` work
//! cross-language without callers having to learn per-language target-kind
//! routing rules.

use crate::framework_helpers::{enclosing_class, span_contains};
use crate::resolution::index::SymbolTable;
use graph_nexus_core::analyzer::types::{LocalGraph, RawNode};
use graph_nexus_core::graph::{Edge, NodeKind, RelType};
use graph_nexus_core::pool::StringPool;

/// Sentinel prefix the Rust parser writes into `RawNode.heritage` for
/// methods defined inside an `impl` block. Carries the impl target's type
/// name (e.g. `__impl_target__:Resolver`). Post-process strips the prefix
/// and uses the suffix as the Class name to bridge struct ↔ impl.
pub const IMPL_TARGET_PREFIX: &str = "__impl_target__:";

/// Emit `HasMethod` / `HasProperty` edges by walking the raw per-file
/// `LocalGraph`s and resolving names back to node indices via `SymbolTable`.
/// Returns the number of edges appended to `edges_out`.
///
/// Idempotent w.r.t. the input — re-running on the same `local_graphs` would
/// emit the same edge set. Callers must ensure they don't double-invoke.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add("post_process:class_membership");
    let reason_impl = string_pool.add("post_process:class_membership:rust_impl");

    let mut emitted = 0usize;

    for local_graph in local_graphs {
        let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
        let raws = &local_graph.nodes;

        emitted += emit_pass1_span(raws, &path_str, symbol_table, reason, edges_out);
        emitted += emit_pass2_rust_impl(raws, &path_str, symbol_table, reason_impl, edges_out);
    }

    emitted
}

/// Pass 1 — cross-language span containment.
///
/// For each Class node `c` in the file, find the innermost-enclosing Class for
/// every Function/Method/Property in the same file. If that innermost Class is
/// `c`, emit the corresponding `HasMethod` (Function|Method) or `HasProperty`
/// (Property) edge from `c` to the member.
fn emit_pass1_span(
    raws: &[RawNode],
    path_str: &str,
    symbol_table: &SymbolTable,
    reason: graph_nexus_core::pool::StrRef,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let mut emitted = 0usize;

    for member in raws {
        let rel_type = match member.kind {
            NodeKind::Function | NodeKind::Method | NodeKind::Constructor => RelType::HasMethod,
            NodeKind::Property => RelType::HasProperty,
            _ => continue,
        };

        // Innermost enclosing class — handles nested-class case via
        // `min_by_key(span_area)` inside the helper.
        let Some((class_name, _)) = enclosing_class(raws, member.span) else {
            continue;
        };

        let Some(class_idx) = symbol_table.lookup_in_file(path_str, &class_name) else {
            continue;
        };
        let Some(member_idx) = symbol_table.lookup_in_file(path_str, &member.name) else {
            continue;
        };

        // SymbolTable `file_scoped` maps (file, name) → single node_id with
        // last-write-wins. When a member has the same name as its enclosing
        // class — Java/C# `class Foo { public Foo() {} }` constructors are
        // the canonical case — both lookups resolve to the same idx (the
        // later-registered Method overwrites the Class). Emitting would
        // create a self-loop. Skip; documented limitation pending a
        // future SymbolTable change that handles multi-id-per-name.
        if class_idx == member_idx {
            continue;
        }

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

/// Pass 2 — Rust `impl` bridge.
///
/// The Rust parser stamps each impl method's owning type name into the
/// method's `heritage` field as `__impl_target__:Foo`. We scan for that
/// sentinel and emit a `HasMethod` edge from the named Class to the method,
/// even though the method's span is OUTSIDE the Class span (Rust splits
/// `struct Foo {}` from `impl Foo { fn bar() {} }`).
///
/// Skip emission when Pass 1 already covered the link — checked by
/// span containment: if the method's span lies inside any Class span in
/// the same file with `class_name`, Pass 1 already emitted.
fn emit_pass2_rust_impl(
    raws: &[RawNode],
    path_str: &str,
    symbol_table: &SymbolTable,
    reason: graph_nexus_core::pool::StrRef,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let mut emitted = 0usize;

    for member in raws {
        if !matches!(member.kind, NodeKind::Function | NodeKind::Method | NodeKind::Constructor) {
            continue;
        }
        for tag in &member.heritage {
            let Some(class_name) = tag.strip_prefix(IMPL_TARGET_PREFIX) else {
                continue;
            };

            // Skip if Pass 1 would have covered: any Class node in this file
            // with matching name whose span contains the member span.
            let already_via_span = raws.iter().any(|n| {
                matches!(n.kind, NodeKind::Class)
                    && n.name == class_name
                    && span_contains(n.span, member.span)
            });
            if already_via_span {
                continue;
            }

            let Some(class_idx) = symbol_table.lookup_in_file(path_str, class_name) else {
                continue;
            };
            let Some(member_idx) = symbol_table.lookup_in_file(path_str, &member.name) else {
                continue;
            };

            edges_out.push(Edge {
                source: class_idx,
                target: member_idx,
                rel_type: RelType::HasMethod,
                confidence: 1.0,
                reason,
            });
            emitted += 1;
        }
    }

    emitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::analyzer::types::LocalGraph;
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
        }
    }

    fn raw_with_heritage(
        name: &str,
        kind: NodeKind,
        span: (u32, u32, u32, u32),
        heritage: Vec<String>,
    ) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage,
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
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
        emit_edges(&local_graphs, &symbol_table, &mut string_pool, &mut edges);
        edges
    }

    fn lg(path: &str, nodes: Vec<RawNode>) -> LocalGraph {
        LocalGraph {
            file_path: PathBuf::from(path),
            content_hash: [0u8; 32],
            nodes,
            documents: Vec::new(),
            imports: Vec::new(),
            routes: Vec::new(),
            framework_refs: Vec::new(),
            fanout_refs: Vec::new(),
            blind_spots: Vec::new(),
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
        assert!(edges.iter().all(|e| matches!(e.rel_type, RelType::HasProperty)));
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
    fn rust_inherent_impl_via_impl_map() {
        // struct Foo (1..1) — single line decl
        // fn new (5..7) heritage=__impl_target__:Foo, kind=Function
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 1, 12)),
            raw_with_heritage(
                "new",
                NodeKind::Function,
                (5, 4, 7, 4),
                vec!["__impl_target__:Foo".into()],
            ),
        ];
        let edges = run(vec![lg("foo.rs", nodes)]);
        assert_eq!(edges.len(), 1);
        assert!(matches!(edges[0].rel_type, RelType::HasMethod));
        assert_eq!(edges[0].source, 0); // Foo
        assert_eq!(edges[0].target, 1); // new
    }

    #[test]
    fn rust_trait_impl_via_impl_map() {
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 1, 12)),
            raw_with_heritage(
                "fmt",
                NodeKind::Method,
                (5, 4, 7, 4),
                vec!["__impl_target__:Foo".into()],
            ),
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
    fn java_style_same_named_constructor_is_skipped() {
        // Java emits `class Foo { public Foo() {} }` as Class "Foo" containing
        // Method "Foo". SymbolTable file_scoped is name→single-id; the
        // later-registered Method overwrites the Class, so lookup_in_file
        // returns the same idx for both. Skip to avoid emitting a self-loop.
        // Pin current behaviour so a future SymbolTable change that handles
        // multi-id-per-name flips this expectation deliberately, not silently.
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 10, 0)),
            raw("Foo", NodeKind::Method, (2, 4, 4, 4)),
        ];
        let edges = run(vec![lg("Foo.java", nodes)]);
        assert_eq!(
            edges.len(),
            0,
            "same-name collision must NOT emit a self-loop edge"
        );
    }

    #[test]
    fn rust_pass2_skips_when_pass1_covered() {
        // If a Class's span DOES contain the method's span AND heritage has
        // __impl_target__, Pass 1 emits and Pass 2 must skip (no duplicate).
        let nodes = vec![
            raw("Foo", NodeKind::Class, (1, 0, 10, 0)),
            raw_with_heritage(
                "bar",
                NodeKind::Method,
                (3, 4, 5, 4),
                vec!["__impl_target__:Foo".into()],
            ),
        ];
        let edges = run(vec![lg("foo.rs", nodes)]);
        assert_eq!(edges.len(), 1, "must not double-emit");
    }
}
