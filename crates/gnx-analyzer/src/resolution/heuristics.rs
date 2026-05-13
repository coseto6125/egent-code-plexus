use gnx_core::analyzer::types::RawImport;
use gnx_core::graph::RelType;

#[derive(Debug, Clone)]
pub struct ResolvedEdge {
    pub source_node_name: String,
    pub target_node_name: String,
    pub rel_type: RelType,
    pub confidence: f32,
    pub reason: String,
}

/// Translates RawImport into concrete edges with confidence scores
/// matching original GitNexus TypeScript heuristics.
pub fn resolve_imports(
    imports: &[RawImport],
    source_node_name: &str,
) -> Vec<ResolvedEdge> {
    imports.iter().map(|import| {
        // Original GitNexus TypeScript heuristics for import confidence
        let confidence = if import.source.starts_with('.') {
            1.0 // Exact local path
        } else if import.source.starts_with('@') {
            0.9 // Monorepo package or path alias
        } else if import.source.contains('/') {
            0.8 // Deep path within a package
        } else {
            0.5 // Standard library or root level third-party package
        };

        ResolvedEdge {
            source_node_name: source_node_name.to_string(),
            target_node_name: import.imported_name.clone(),
            rel_type: RelType::Calls, // Translates into concrete CALLS edges
            confidence,
            reason: format!("Resolved from import: {}", import.source),
        }
    }).collect()
}
