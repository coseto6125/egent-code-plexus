use gnx_core::analyzer::types::RawImport;
use std::path::Path;

use crate::resolution::heuristics::ResolutionTier;
use crate::resolution::index::SymbolTable;

pub type NodeId = u32;

/// The core resolver engine that matches symbol names to concrete global nodes.
pub struct Resolver<'a> {
    symbol_table: &'a SymbolTable,
}

impl<'a> Resolver<'a> {
    /// Creates a new `Resolver` with a reference to the global `SymbolTable`.
    pub fn new(symbol_table: &'a SymbolTable) -> Self {
        Self { symbol_table }
    }

    /// Resolves a symbol name to possible target nodes with confidence scores.
    pub fn resolve_symbol(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
    ) -> Vec<(NodeId, f32)> {
        let mut results = Vec::new();
        let source_file_str = source_file.to_string_lossy();

        // Tier 1: Try SameFile
        if let Some(node_id) = self
            .symbol_table
            .lookup_in_file(&source_file_str, symbol_name)
        {
            results.push((node_id, ResolutionTier::SameFile.base_confidence()));
            return results; // Highest precedence, return early
        }

        // Tier 2: Try ImportScoped
        for import in raw_imports {
            let is_match = match &import.alias {
                Some(alias) => alias == symbol_name,
                None => import.imported_name == symbol_name,
            };

            if is_match {
                // The actual name exported by the source file
                let exported_name = &import.imported_name;

                if let Some(node_id) = self
                    .symbol_table
                    .lookup_in_file(&import.source, exported_name)
                {
                    results.push((node_id, ResolutionTier::ImportScoped.base_confidence()));
                    return results;
                }
            }
        }

        // Tier 3: Try Global (Fallback)
        let global_matches = self.symbol_table.lookup_global(symbol_name);
        if !global_matches.is_empty() {
            // For now, if there are multiple global matches, we just push the first one or all
            // To match original behavior we push all with Global confidence
            for node_id in global_matches {
                results.push((node_id, ResolutionTier::Global.base_confidence()));
            }
        }

        results
    }
}
