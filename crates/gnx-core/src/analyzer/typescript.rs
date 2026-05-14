use super::provider::LanguageProvider;
use super::types::LocalGraph;
use anyhow::Result;
use std::path::Path;

pub struct TypeScriptProvider;

impl LanguageProvider for TypeScriptProvider {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn parse_file(&self, path: &Path, _source: &[u8]) -> Result<LocalGraph> {
        // Minimal mock implementation for the pipeline MVP.
        // Full tree-sitter extraction logic matching original GitNexus behavior
        // will be added in the next planning phase.
        Ok(LocalGraph {
            content_hash: [0; 32],
            file_path: path.to_path_buf(),
            nodes: vec![],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        })
    }
}
