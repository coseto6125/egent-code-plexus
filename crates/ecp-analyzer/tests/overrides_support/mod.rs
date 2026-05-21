//! Shared helpers for `overrides_{java,kotlin,csharp,cpp,dart,swift}` tests.
//! The `parse()` wrapper stays per-test (provider type differs); everything
//! else — symbol-table build, edge filter, decorator predicate — is identical.
//!
//! Each integration-test crate compiles its own copy of this module, so some
//! callers won't use `has_decorator` — `allow(dead_code)` keeps the warning
//! quiet across all six dialects.
#![allow(dead_code)]

use ecp_analyzer::post_process::overrides;
use ecp_analyzer::resolution::index::SymbolTable;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::RelType;
use ecp_core::pool::StringPool;

pub fn build_symbol_table(local_graphs: &[LocalGraph]) -> SymbolTable {
    let mut st = SymbolTable::new();
    let mut current = 0u32;
    for lg in local_graphs {
        let path_str = lg.file_path.to_string_lossy().replace('\\', "/");
        for rn in &lg.nodes {
            st.register_node(&path_str, &rn.name, current, rn.kind);
            current += 1;
        }
    }
    st
}

pub fn run_overrides(local_graphs: &[LocalGraph]) -> Vec<(u32, u32)> {
    let st = build_symbol_table(local_graphs);
    let mut sp = StringPool::new();
    let mut edges = Vec::new();
    overrides::emit_edges(local_graphs, &st, &mut sp, &mut edges);
    edges
        .into_iter()
        .filter(|e| matches!(e.rel_type, RelType::Overrides))
        .map(|e| (e.source, e.target))
        .collect()
}

pub fn has_decorator(node: &RawNode, d: &str) -> bool {
    node.decorators.iter().any(|dec| dec.contains(d))
}
