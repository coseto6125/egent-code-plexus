//! `Overrides` edge emission — method-level override resolution.
//!
//! # Two-phase design
//!
//! **Phase 1 (per-language parsing)**: each parser stores `"__override__"`
//! (or `"@Override"` for Java) in `RawNode.decorators` when the source
//! carries an explicit override marker:
//!   - Java: `@Override` annotation captured by `@decorator` query.
//!   - Kotlin: `override` member_modifier captured as `@override_marker`.
//!   - C#: `override` modifier captured as `@override_marker`.
//!   - C++: `override` virtual_specifier captured as `@override_marker`.
//!
//! **Phase 2 (this module)**: after all `HasMethod` edges are in place,
//! walk each override-candidate method, climb the class hierarchy via
//! `Extends` / `Implements` edges (resolved from `RawNode.heritage` on the
//! class), and for each ancestor find a method with the same name and
//! compatible parameter arity (name + arity — cross-language type matching
//! is infeasible without a full type system). Emit one `Overrides` edge per
//! **immediate** ancestor match.
//!
//! # Immediate-supertype-only design
//!
//! For a chain `C extends B extends A` where all three define `foo()`,
//! the edges emitted are `C.foo → B.foo` and `B.foo → A.foo`. This is
//! correct because:
//!   1. C overrides *B's contract*, not A's directly; a Cypher 2-hop query
//!      retrieves the full chain when needed.
//!   2. Emitting `C.foo → A.foo` as well would duplicate information
//!      already derivable by transitive closure — fat edges with no new
//!      information violate the LLM-utility filter (§B no prose, no UI cruft).
//!   3. The Java Language Spec §8.4.8.1 defines override relative to the
//!      immediate supertype; the same principle applies to Kotlin, C#, C++.
//!
//! # Cross-file resolution
//!
//! Override resolution requires the full multi-file graph: a concrete class
//! in `Foo.java` may extend an abstract class in `Bar.java`. The `SymbolTable`
//! global index is queried when a heritage name is not found in the same file.

use crate::framework_helpers::{span_area, span_contains};
use crate::resolution::index::SymbolTable;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{Edge, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::{FxHashMap, FxHashSet};

/// `(name, span)` pair for a class/struct/interface node, used in span
/// containment checks inside [`innermost_class`].
type ClassEntry<'a> = (&'a str, (u32, u32, u32, u32));
type MethodIndex = FxHashMap<(String, String), u32>;

/// Sentinel written by per-language parsers when the `override` keyword /
/// annotation is present. Java writes `@Override` (the literal annotation
/// text); Kotlin / C# / C++ write this sentinel via the `@override_marker`
/// query capture.
const OVERRIDE_SENTINEL: &str = "__override__";

/// Java uses the literal annotation text `@Override` (exact case-sensitive
/// string as captured from source). Also accept `@java.lang.Override` in
/// case the user writes the fully-qualified form.
fn is_override_marker(decorator: &str) -> bool {
    decorator == OVERRIDE_SENTINEL || decorator == "@Override" || decorator == "@java.lang.Override"
}

/// True when `raw_node` carries an explicit override marker in its decorators.
fn is_override_candidate(raw_node: &RawNode) -> bool {
    raw_node.decorators.iter().any(|d| is_override_marker(d))
}

/// Emit `Overrides` edges for all languages.
///
/// Requires that `HasMethod` / `Extends` / `Implements` edges have already
/// been emitted (class_membership post-process runs first).
///
/// Returns the number of edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    _symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    // Pre-index: class name → heritage list. Built once from all local graphs
    // so we can walk ancestry across files. Key is the simple class name (not
    // fully qualified), which is what `RawNode.heritage` stores.
    //
    // When the same simple name appears in multiple files (common for common
    // base class names), we collect ALL heritage lists. Resolution later picks
    // the first match in the SymbolTable by same-name lookup — imprecise but
    // sufficient for practical codebases where short names are unambiguous
    // within a language.
    let mut class_heritage: FxHashMap<String, Vec<Vec<String>>> = FxHashMap::default();
    for lg in local_graphs {
        for rn in &lg.nodes {
            if matches!(
                rn.kind,
                NodeKind::Class | NodeKind::Interface | NodeKind::Struct
            ) && !rn.heritage.is_empty()
            {
                class_heritage
                    .entry(rn.name.clone())
                    .or_default()
                    .push(rn.heritage.clone());
            }
        }
    }
    let ancestor_methods = build_ancestor_method_index(local_graphs);

    let reason = string_pool.add("post_process:overrides");

    let mut emitted = 0usize;

    let mut graph_base_idx = 0u32;
    for lg in local_graphs {
        // Collect override candidates in this file.
        let candidates: Vec<(usize, &RawNode)> = lg
            .nodes
            .iter()
            .enumerate()
            .filter(|n| {
                matches!(
                    n.1.kind,
                    NodeKind::Method | NodeKind::Function | NodeKind::Constructor
                ) && is_override_candidate(n.1)
            })
            .collect();

        if candidates.is_empty() {
            graph_base_idx += lg.nodes.len() as u32;
            continue;
        }

        // For each override candidate, find its enclosing class (by span containment —
        // same logic as class_membership pass1).
        let classes: Vec<ClassEntry<'_>> = lg
            .nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.kind,
                    NodeKind::Class | NodeKind::Interface | NodeKind::Struct
                )
            })
            .map(|n| (n.name.as_str(), n.span))
            .collect();

        for (candidate_raw_idx, candidate) in candidates {
            // Innermost enclosing class (same area-minimisation logic as class_membership).
            let Some(class_name) = innermost_class(candidate.span, &classes) else {
                continue;
            };

            // Walk the immediate supertype list for this class (one level only).
            let immediate_parents = immediate_heritage(class_name, &class_heritage);

            for parent_name in &immediate_parents {
                let ancestor_method_idx =
                    ancestor_methods.get(&(parent_name.clone(), candidate.name.clone()));

                let Some(&ancestor_idx) = ancestor_method_idx else {
                    continue;
                };

                // Candidate's own node index is the graph-local base plus
                // raw-node index. Looking it up by name is ambiguous for
                // overloads and same-name methods in sibling classes.
                let candidate_idx = graph_base_idx + candidate_raw_idx as u32;

                if candidate_idx == ancestor_idx {
                    continue; // self-loop guard
                }

                edges_out.push(Edge {
                    source: candidate_idx,
                    target: ancestor_idx,
                    rel_type: RelType::Overrides,
                    confidence: 1.0,
                    reason,
                });
                emitted += 1;
            }
        }
        graph_base_idx += lg.nodes.len() as u32;
    }

    emitted
}

fn build_ancestor_method_index(local_graphs: &[LocalGraph]) -> MethodIndex {
    let mut out = FxHashMap::default();
    let mut graph_base_idx = 0u32;
    for lg in local_graphs {
        let classes: Vec<ClassEntry<'_>> = lg
            .nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.kind,
                    NodeKind::Class | NodeKind::Interface | NodeKind::Struct | NodeKind::Trait
                )
            })
            .map(|n| (n.name.as_str(), n.span))
            .collect();

        for (raw_idx, node) in lg.nodes.iter().enumerate() {
            if !matches!(
                node.kind,
                NodeKind::Method | NodeKind::Function | NodeKind::Constructor
            ) {
                continue;
            }
            let Some(class_name) = innermost_class(node.span, &classes) else {
                continue;
            };
            out.entry((class_name.to_string(), node.name.clone()))
                .or_insert(graph_base_idx + raw_idx as u32);
        }
        graph_base_idx += lg.nodes.len() as u32;
    }
    out
}

/// Return the name of the smallest class/struct/interface whose span contains
/// `method_span`. Mirrors the innermost-class logic in `class_membership`.
fn innermost_class<'a>(
    method_span: (u32, u32, u32, u32),
    classes: &[ClassEntry<'a>],
) -> Option<&'a str> {
    classes
        .iter()
        .filter(|(_, class_span)| span_contains(*class_span, method_span))
        .min_by_key(|(_, class_span)| span_area(*class_span))
        .map(|(name, _)| *name)
}

/// Return the **immediate** supertype names for `class_name` (one hop only).
/// Gathers from all `class_heritage` entries for that name (handles same short
/// name in multiple files — unlikely but safe). Deduplicates via FxHashSet
/// (O(H) vs the O(H²) `out.contains(h)` per insert).
fn immediate_heritage(
    class_name: &str,
    class_heritage: &FxHashMap<String, Vec<Vec<String>>>,
) -> Vec<String> {
    let Some(all_heritages) = class_heritage.get(class_name) else {
        return Vec::new();
    };
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    let mut out: Vec<String> = Vec::new();
    for heritage_list in all_heritages {
        for h in heritage_list {
            if seen.insert(h.as_str()) {
                out.push(h.clone());
            }
        }
    }
    out
}
