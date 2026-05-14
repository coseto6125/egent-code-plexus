use crate::analyzer::types::LocalGraph;
use std::path::Path;

pub trait LanguageProvider: Send + Sync {
    /// The language name (e.g., "typescript")
    fn name(&self) -> &'static str;

    /// Parse a source file and extract the LocalGraph
    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph>;
}
