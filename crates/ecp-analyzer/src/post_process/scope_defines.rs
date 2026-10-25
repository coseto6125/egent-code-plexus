//! `Defines` edge emission — scope-containment fill for File / Namespace / Module.
//!
//! Covers scope relationships NOT already handled by `HasMethod` / `HasProperty`:
//!
//! - `File → top-level symbol` (any non-member kind whose `owner_class.is_none()`)
//! - `Namespace → child` (`owner_class == Some(namespace_name)`)
//! - `Module → child` (`owner_class == Some(module_name)`)
//!
//! Class → Method / Property containment stays exclusively HasMethod / HasProperty
//! — Defines must NOT duplicate. The invariant: `owner_class.is_none()` OR the
//! owner is a Namespace/Module (not a Class/Interface/Trait).
//!
//! Both ends come from raw indexes through the build's `RawIndexMap`, never
//! from a name lookup: two same-name declarations each get their own edge.
//!
//! A scoped node (a Rust nested `fn`, a closure node) has no owner, yet no
//! File or module declares it: its callable does, through the lexical-parent
//! table. It gets no `Defines` edge.

use ecp_core::analyzer::types::{span_contains, LocalGraph};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::graph::{Edge, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::FxHashMap;

/// Emit `Defines` edges from File / Namespace / Module nodes to their
/// contained symbols. `is_scoped` tells a scoped node by its node id; it
/// gets no edge. Returns the number of edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    file_node_idx: &FxHashMap<String, u32>,
    raw_map: &RawIndexMap,
    is_scoped: impl Fn(u32) -> bool,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add("pass2:defines");
    let mut emitted = 0usize;

    for (lg_idx, local_graph) in local_graphs.iter().enumerate() {
        let file_path_lossy = local_graph.file_path.to_string_lossy().replace('\\', "/");
        let file = FileOrd(lg_idx as u32);
        let nodes = &local_graph.nodes;

        let Some(&file_node_id) = file_node_idx.get(&file_path_lossy) else {
            continue;
        };

        // Namespace/Module raw indexes by name, in source order, so a child
        // finds its container by its owner name.
        let mut container_ids: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
        for (i, n) in nodes.iter().enumerate() {
            if matches!(n.kind, NodeKind::Namespace | NodeKind::Module) {
                container_ids.entry(n.name.as_str()).or_default().push(i);
            }
        }

        for (raw_idx, raw_node) in nodes.iter().enumerate() {
            let kind = raw_node.kind;

            // Only emit Defines for kinds that represent scope boundaries.
            // Property / Constructor are covered by HasMethod / HasProperty.
            // Method is included here because some parsers (Ruby, PHP) emit
            // top-level `def` as NodeKind::Method with owner_class=None —
            // those are module-level callables, not class members.
            // When owner_class.is_some() the Method will be caught by
            // HasMethod in class_membership, so no duplication is possible.
            let is_definable = matches!(
                kind,
                NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Class
                    | NodeKind::Interface
                    | NodeKind::Trait
                    | NodeKind::Const
                    | NodeKind::Variable
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Typedef
                    | NodeKind::Macro
                    | NodeKind::Module
                    | NodeKind::Namespace
            );
            if !is_definable {
                continue;
            }

            let Some(child_id) = raw_map
                .node(file, RawIdx(raw_idx as u32))
                .filter(|&id| !is_scoped(id))
            else {
                continue;
            };

            match &raw_node.owner_class {
                None => {
                    // Top-level symbol: File → symbol.
                    edges_out.push(Edge {
                        source: file_node_id,
                        target: child_id,
                        rel_type: RelType::Defines,
                        confidence: 1.0,
                        reason,
                    });
                    emitted += 1;
                }
                Some(owner) => {
                    // Member of a Namespace or Module: container → symbol.
                    // Members of Class/Interface/Trait are covered by HasMethod/HasProperty;
                    // skip them here to preserve the no-duplication invariant.
                    // Of two same-name containers (`namespace Acme` twice), the
                    // innermost one around the child; else the first, as a
                    // statement-form `namespace Acme;` contains nothing.
                    let container = container_ids.get(owner.as_str()).and_then(|ids| {
                        ids.iter()
                            .copied()
                            .filter(|&c| {
                                c != raw_idx && span_contains(nodes[c].span, raw_node.span)
                            })
                            .max_by_key(|&c| (nodes[c].span.0, nodes[c].span.1))
                            .or_else(|| ids.first().copied())
                    });
                    if let Some(container_id) =
                        container.and_then(|c| raw_map.node(file, RawIdx(c as u32)))
                    {
                        edges_out.push(Edge {
                            source: container_id,
                            target: child_id,
                            rel_type: RelType::Defines,
                            confidence: 1.0,
                            reason,
                        });
                        emitted += 1;
                    }
                    // If the owner is not in container_ids it's a Class/Interface/Trait —
                    // those are covered by HasMethod/HasProperty, so we silently skip.
                }
            }
        }
    }

    emitted
}
