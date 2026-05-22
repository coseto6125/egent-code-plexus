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

use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::FxHashMap;

/// Emit `Defines` edges from File / Namespace / Module nodes to their
/// contained symbols. Returns the number of edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    file_node_idx: &FxHashMap<String, u32>,
    symbol_table: &crate::resolution::index::SymbolTable,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let reason = string_pool.add("pass2:defines");
    let mut emitted = 0usize;

    for local_graph in local_graphs {
        let file_path_lossy = local_graph.file_path.to_string_lossy().replace('\\', "/");

        let Some(&file_node_id) = file_node_idx.get(&file_path_lossy) else {
            continue;
        };

        // Collect namespace/module node IDs once so Pass 2 can look them up by name.
        // Maps container_name → node_id for each Namespace/Module in this file.
        let container_ids: FxHashMap<&str, u32> = local_graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Namespace | NodeKind::Module))
            .filter_map(|n| {
                symbol_table
                    .lookup_in_file(&file_path_lossy, &n.name)
                    .map(|id| (n.name.as_str(), id))
            })
            .collect();

        for raw_node in &local_graph.nodes {
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

            let Some(child_id) = symbol_table.lookup_in_file(&file_path_lossy, &raw_node.name)
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
                    if let Some(&container_id) = container_ids.get(owner.as_str()) {
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
