//! `Decorates` edge emission — walks every node's `RawNode.decorators`,
//! resolves each decorator name via the `Resolver`, and emits an edge from
//! the decorated symbol to the resolved target or a synthetic `Annotation`
//! node.
//!
//! Resolution strategy (first-hit wins):
//!   1. Normalize the raw decorator string via `normalize_decorator` (strips
//!      `@`, `#[`, `[]`, `Attribute` suffix, expands `derive`).
//!   2. Run `Resolver::resolve_symbol(lookup_name, ResolveTarget::Type)`.
//!   3. On hit: emit `Decorates` edge from decorated node → resolved target.
//!   4. On miss: emit a synthetic `NodeKind::Annotation` node (deduped by
//!      `full_name` across the whole graph via an `FxHashMap`) and emit
//!      `Decorates` edge from decorated node → synthetic node.
//!
//! Deduplication of synthetic Annotation nodes: one node per unique
//! `full_name` string across ALL local graphs. The dedup map is keyed by
//! `full_name` (the canonical dotted/fully-qualified form), not `lookup_name`,
//! so `@staticmethod` and `@functools.cached_property` get distinct nodes
//! even though both have `lookup_name` with the same last segment if they
//! happened to conflict (they don't, but the invariant holds in general).
//!
//! `file_idx` for synthetic nodes is set to `u32::MAX` (sentinel for "no
//! single owning file") — consistent with `EventTopic` synthetic nodes.
//!
//! Languages wired: Python / TypeScript / JavaScript / Java / Kotlin /
//! C# / Rust / PHP / Swift / Dart. Deferred: Go / Ruby / C / C++.

use crate::framework_helpers::normalize_decorator;
use crate::resolution::index::ResolveTarget;
use crate::resolution::resolver::Resolver;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use ecp_core::uid;
use rustc_hash::{FxHashMap, FxHashSet};

/// Emit `Decorates` edges for all local graphs.
///
/// `nodes` is extended with synthetic `Annotation` nodes for unresolved
/// decorators (deduped by full name). Returns the number of `Decorates`
/// edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    resolver: &Resolver<'_>,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add("post_process:decorates");

    // Dedup map: decorator full_name → synthetic node index.
    // Keyed by full_name (dotted canonical form) so `functools.cached_property`
    // and `cached_property` remain distinct if both appear.
    let mut synthetic: FxHashMap<String, u32> = FxHashMap::default();

    let mut emitted = 0usize;
    // (source_node_idx, target_node_idx) pair dedup to avoid duplicate edges
    // when the same decorator appears more than once on a node (e.g. some
    // parsers may emit duplicates; FxHashSet is cheaper than Vec contains).
    let mut edge_dedup: FxHashSet<(u32, u32)> = FxHashSet::default();

    let mut graph_base_idx = 0u32;
    for local_graph in local_graphs {
        for (raw_idx, raw_node) in local_graph.nodes.iter().enumerate() {
            if raw_node.decorators.is_empty() {
                continue;
            }
            // Only emit Decorates from symbols that can carry decorators.
            if !matches!(
                raw_node.kind,
                NodeKind::Class
                    | NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Constructor
                    | NodeKind::Property
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Interface
                    | NodeKind::Trait
            ) {
                continue;
            }

            let source_idx = graph_base_idx + raw_idx as u32;

            for raw_dec in &raw_node.decorators {
                // Skip the override sentinel injected by the Kotlin/C#/C++ parsers —
                // that's consumed by the Overrides post-process, not here.
                if raw_dec == "__override__" {
                    continue;
                }

                let pairs = normalize_decorator(raw_dec);

                for (lookup_name, full_name) in pairs {
                    if lookup_name.is_empty() {
                        continue;
                    }

                    // Resolve via SymbolTable — ResolveTarget::Type covers
                    // Annotation classes (Java @interface, C# Attribute subclass,
                    // Kotlin annotation class, etc.).
                    let resolved = resolver.resolve_symbol(
                        &local_graph.file_path,
                        &lookup_name,
                        &local_graph.imports,
                        ResolveTarget::Type,
                    );

                    if !resolved.is_empty() {
                        for (target_id, confidence) in resolved {
                            if edge_dedup.insert((source_idx, target_id)) {
                                edges.push(Edge {
                                    source: source_idx,
                                    target: target_id,
                                    rel_type: RelType::Decorates,
                                    confidence,
                                    reason,
                                });
                                emitted += 1;
                            }
                        }
                    } else {
                        // Resolver miss — emit synthetic Annotation node (deduped).
                        let synthetic_idx =
                            *synthetic.entry(full_name.clone()).or_insert_with(|| {
                                let idx = nodes.len() as u32;
                                // Synthetic path uses the full_name so that
                                // uid::compute produces a stable hash across builds.
                                let synthetic_path = format!("<annotation>/{}", full_name);
                                let node_uid = uid::compute(
                                    NodeKind::Annotation,
                                    &synthetic_path,
                                    None,
                                    &full_name,
                                );
                                let name_ref = string_pool.add(&full_name);
                                nodes.push(Node {
                                    uid: node_uid,
                                    name: name_ref,
                                    // u32::MAX: no single owning file (same sentinel as
                                    // EventTopic synthetic nodes).
                                    file_idx: u32::MAX,
                                    kind: NodeKind::Annotation,
                                    span: (0, 0, 0, 0),
                                    community_id: 0,
                                    owner_class: ecp_core::pool::StrRef::default(),
                                    content_hash: 0,
                                });
                                idx
                            });

                        if edge_dedup.insert((source_idx, synthetic_idx)) {
                            edges.push(Edge {
                                source: source_idx,
                                target: synthetic_idx,
                                rel_type: RelType::Decorates,
                                // Synthetic nodes are unresolved fallbacks — lower
                                // confidence than a SymbolTable hit but high enough
                                // that the edge is still useful for traversal.
                                confidence: 0.8,
                                reason,
                            });
                            emitted += 1;
                        }
                    }
                }
            }
        }

        graph_base_idx += local_graph.nodes.len() as u32;
    }

    emitted
}
