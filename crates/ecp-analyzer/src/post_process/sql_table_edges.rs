//! `QueriesTable` edge emission from `LocalGraph.sql_refs`.
//!
//! Each `RawSqlRef` resolves to: the enclosing Function/Method (via
//! `SymbolTable::lookup_in_file`, same-language same-file) → the referenced
//! table's `Class` node (via a local name→idx index over `nodes`, because the
//! table is a cross-language `sql` node the SymbolTable's language barrier would
//! otherwise hide). Emits one `QueriesTable` edge per (symbol, table) with
//! `reason = verb`.
//!
//! Honest-no-data: an unresolved ref, an unresolvable enclosing symbol, or a
//! table name with no matching `Class` node yields NO edge — never a fabricated
//! source or sink. Returns the emitted-edge count for builder telemetry.

use crate::resolution::index::SymbolTable;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::FxHashMap;

pub fn emit_edges(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
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
    for lg in local_graphs.iter() {
        let Some(ref refs) = lg.sql_refs else {
            continue;
        };
        if refs.is_empty() {
            continue;
        }
        let path_str = lg.file_path.to_string_lossy().replace('\\', "/");

        for raw in refs.iter() {
            if raw.unresolved {
                // BlindSpot handled in a later task; no edge here.
                continue;
            }
            let Some(sym_name) = raw.enclosing_symbol.as_deref() else {
                // Module-top-level SQL: no caller to attribute.
                continue;
            };
            let Some(src_idx) = symbol_table.lookup_in_file(&path_str, sym_name) else {
                continue;
            };
            for (table, verb) in raw.tables.iter() {
                let Some(&tgt_idx) = table_idx.get(table) else {
                    // Unknown table → drop; no fabrication.
                    continue;
                };
                let reason_ref = string_pool.add(verb.as_reason());
                edges.push(Edge {
                    source: src_idx,
                    target: tgt_idx,
                    rel_type: RelType::QueriesTable,
                    confidence: 1.0,
                    reason: reason_ref,
                });
                edge_count += 1;
            }
        }
    }
    edge_count
}
