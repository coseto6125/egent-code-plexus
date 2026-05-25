//! Saga compensation pair detection: `CompensatedBy` edge emission.
//!
//! ## What this pass does
//!
//! Detects compensator function names following the Saga convention and pairs
//! them with their corresponding operation nodes, emitting
//! `RelType::CompensatedBy` edges (Task 3).
//!
//! A *compensator* is a function whose name starts with one of the roots
//! `compensate`, `undo`, or `rollback`, followed by an underscore separator
//! (snake_case) or a case boundary (camelCase / PascalCase), and then the
//! operation name suffix.
//!
//! ## Case handling
//!
//! | Input name        | Root    | Recovered operation |
//! |-------------------|---------|---------------------|
//! | `undo_book_room`  | `undo`  | `book_room`         |
//! | `undoBookRoom`    | `undo`  | `bookRoom`          |
//! | `UndoBookRoom`    | `Undo`  | `BookRoom`          |
//!
//! The recovered operation name preserves the suffix's **original case** so
//! that it matches the operation node's name verbatim in the graph.

use ecp_core::graph::{Edge, Node, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::{FxHashMap, FxHashSet};

/// Compensator roots, lower-cased. Matched as a prefix on the lower-cased name.
const COMPENSATOR_ROOTS: &[&str] = &["compensate", "undo", "rollback"];

/// Result of stripping a compensator root: the bare operation name with its
/// ORIGINAL case preserved (so it matches the operation node's name).
#[derive(Debug, PartialEq, Eq)]
pub struct CompensatorMatch {
    pub operation_name: String,
}

/// If `name` is a compensator (`<root>` followed by a `_`-separator or a
/// case-boundary), return the bare operation name with original case. Else None.
pub fn strip_compensator_root(name: &str) -> Option<CompensatorMatch> {
    let lower = name.to_ascii_lowercase();
    for &root in COMPENSATOR_ROOTS {
        if !lower.starts_with(root) {
            continue;
        }
        let rest = &name[root.len()..];
        if rest.is_empty() {
            continue; // root with no suffix
        }
        // snake_case separator
        if let Some(suffix) = rest.strip_prefix('_') {
            if !suffix.is_empty() {
                return Some(CompensatorMatch {
                    operation_name: suffix.to_string(),
                });
            }
            continue;
        }
        // camel/Pascal boundary: next char must start a new word (uppercase).
        let first = rest.chars().next().unwrap();
        if first.is_ascii_uppercase() {
            let compensator_first = name.chars().next().unwrap();
            if compensator_first.is_ascii_uppercase() {
                // PascalCase: operation keeps its capital (BookRoom).
                return Some(CompensatorMatch {
                    operation_name: rest.to_string(),
                });
            }
            // camelCase: lowercase the operation's first char (bookRoom).
            let mut chars = rest.chars();
            let lowered: String = chars
                .next()
                .map(|c| c.to_ascii_lowercase())
                .into_iter()
                .chain(chars)
                .collect();
            return Some(CompensatorMatch {
                operation_name: lowered,
            });
        }
    }
    None
}

/// Emit `CompensatedBy` edges for same-owner-class Saga name-pairs in the node
/// buffer. Returns the count emitted. Adds NO nodes.
///
/// 1. Build a `(src,tgt)` set from existing `Calls` edges (linear scan — CSR
///    offsets don't exist at buffer time).
/// 2. Group callable nodes by `owner_class` (skip empty owner).
/// 3. Within each class build `name -> idx` for O(1) operation lookup.
/// 4. For each compensator, look up the bare operation; emit the edge, conf by
///    calls-back evidence (0.8) else name-only (0.6).
pub fn emit_edges(nodes: &[Node], string_pool: &mut StringPool, edges: &mut Vec<Edge>) -> usize {
    let reason_calls_back = string_pool.add("saga:calls-back");
    let reason_name_only = string_pool.add("saga:name-only");

    let mut calls: FxHashSet<(u32, u32)> = FxHashSet::default();
    for e in edges.iter() {
        if e.rel_type == RelType::Calls {
            calls.insert((e.source, e.target));
        }
    }

    // Group callable nodes by owner_class. Use owned Strings to avoid holding a
    // borrow of `string_pool` across emission.
    let mut by_class: FxHashMap<String, Vec<(u32, String)>> = FxHashMap::default();
    for (idx, node) in nodes.iter().enumerate() {
        if !node.kind.is_callable() {
            continue;
        }
        let owner = string_pool.resolve(&node.owner_class).to_string();
        if owner.is_empty() {
            continue;
        }
        let name = string_pool.resolve(&node.name).to_string();
        by_class.entry(owner).or_default().push((idx as u32, name));
    }

    let mut count = 0usize;
    for members in by_class.values() {
        let name_map: FxHashMap<&str, u32> = members
            .iter()
            .map(|(idx, name)| (name.as_str(), *idx))
            .collect();
        for (comp_idx, comp_name) in members {
            let Some(m) = strip_compensator_root(comp_name) else {
                continue;
            };
            let Some(&op_idx) = name_map.get(m.operation_name.as_str()) else {
                continue;
            };
            let (confidence, reason) = if calls.contains(&(*comp_idx, op_idx)) {
                (0.8_f32, reason_calls_back)
            } else {
                (0.6_f32, reason_name_only)
            };
            edges.push(Edge {
                source: *comp_idx,
                target: op_idx,
                rel_type: RelType::CompensatedBy,
                confidence,
                reason,
            });
            count += 1;
        }
    }
    count
}
