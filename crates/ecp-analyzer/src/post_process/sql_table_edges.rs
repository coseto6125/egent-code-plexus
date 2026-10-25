//! `QueriesTable` edge emission from `LocalGraph.sql_refs`.
//!
//! Each `RawSqlRef` resolves to: its endpoint, the innermost enclosing
//! callable or `Const` by span, else the file (C4 `RecordKind::SqlRef`) → the
//! referenced table's `Class` node (via a local name→idx index over `nodes`,
//! because the table is a cross-language `sql` node the SymbolTable's language
//! barrier would otherwise hide). Emits one `QueriesTable` edge per
//! (endpoint, table) with `reason = verb`. A `File` endpoint's edges go to
//! `file_edges`, as in `path_literal_nodes`.
//!
//! Honest-no-data: an unresolved ref or a table name with no matching `Class`
//! node yields NO edge — never a fabricated source or sink. Returns the
//! emitted-edge count for builder telemetry.

use super::path_literal_nodes::FILE_SOURCE_PENDING;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::{FileOrd, GlobalEndpoint, RawIndexMap};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::FxHashMap;

pub fn emit_edges(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    string_pool: &mut StringPool,
    nodes: &[Node],
    edges: &mut Vec<Edge>,
    file_edges: &mut Vec<(FileOrd, Edge)>,
) -> usize {
    // Language-neutral table index: every Class node keyed by name. The SQL DDL
    // parser emits each table as a Class node; this sidesteps the SymbolTable
    // language barrier that would hide a `sql` node from a Python/Go/etc. caller.
    let mut table_idx: FxHashMap<String, u32> = FxHashMap::default();
    for (i, n) in nodes.iter().enumerate() {
        if n.kind == NodeKind::Class {
            let name = string_pool.resolve(&n.name).to_string();
            table_idx.entry(name).or_insert(i as u32);
        }
    }

    let mut edge_count = 0usize;
    for (lg_idx, lg) in local_graphs.iter().enumerate() {
        let Some(ref refs) = lg.sql_refs else {
            continue;
        };
        if refs.is_empty() {
            continue;
        }
        let file = FileOrd(lg_idx as u32);

        for raw in refs.iter() {
            if raw.unresolved {
                // BlindSpot handled in a later task; no edge here.
                continue;
            }
            let Some(endpoint) = raw.endpoint_in(&lg.nodes) else {
                continue;
            };
            let source = match raw_map.resolve(file, &endpoint) {
                Some(GlobalEndpoint::Node(slot)) => Some(slot),
                Some(GlobalEndpoint::File(_)) => None,
                Some(GlobalEndpoint::Unresolved(_)) | None => continue,
            };
            for (table, verb) in raw.tables.iter() {
                let Some(&tgt_idx) = table_idx.get(table) else {
                    // Unknown table → drop; no fabrication.
                    continue;
                };
                let edge = Edge {
                    source: source.unwrap_or(FILE_SOURCE_PENDING),
                    target: tgt_idx,
                    rel_type: RelType::QueriesTable,
                    confidence: 1.0,
                    reason: string_pool.add(verb.as_reason()),
                };
                match source {
                    Some(_) => edges.push(edge),
                    None => file_edges.push((file, edge)),
                }
                edge_count += 1;
            }
        }
    }
    edge_count
}
