pub mod python;

use graph_nexus_core::analyzer::types::IdentifierRange;

/// Dispatch identifier-occurrence scan to the matching per-language
/// implementation based on `path`'s file extension. Returns an empty
/// vec for unsupported languages — callers treat that as "skip file".
pub fn find_identifier_occurrences(
    path: &str,
    source: &[u8],
    target_name: &str,
) -> Vec<IdentifierRange> {
    if path.to_lowercase().ends_with(".py") {
        return python::find_identifier_occurrences(source, target_name);
    }
    Vec::new()
}
