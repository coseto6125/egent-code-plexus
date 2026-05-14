use crate::graph::NodeKind;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RawNode {
    pub name: String,
    pub kind: NodeKind,
    pub span: (u32, u32, u32, u32),
    pub is_exported: bool,
    pub heritage: Vec<String>, // Base classes, interfaces, traits
    pub type_annotation: Option<String>,
    pub decorators: Vec<String>,
    /// Names of functions/methods invoked from inside this node's body.
    /// Each entry is the callee's *short* name (e.g. `method` for `obj.method()`).
    /// Resolved against imports + same-file symbols in Pass 2 → `RelType::Calls`.
    pub calls: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RawImport {
    pub source: String,
    pub imported_name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RawRoute {
    pub method: String,
    pub path: String,
    pub handler: Option<String>,
    pub span: (u32, u32, u32, u32),
}

#[derive(Debug, Clone)]
pub struct RawDocumentBlock {
    pub name: String,
    pub is_section: bool,
    pub span: (u32, u32, u32, u32),
}

#[derive(Debug, Clone)]
pub struct RawFrameworkRef {
    pub source_name: String,
    pub target_name: String,
    pub confidence: f32,
    pub reason: String,
    pub span: (u32, u32, u32, u32),
}

/// Reflection-style fan-out reference: a single call site whose target cannot
/// be uniquely picked at static-analysis time, but where the analyzer can
/// enumerate the candidate set. The builder emits one `References` edge per
/// candidate with confidence `base_confidence / sqrt(N)` (floored at 0.1).
#[derive(Debug, Clone)]
pub struct RawFanoutRef {
    pub source_name: String,
    pub candidates: Vec<String>,
    pub base_confidence: f32,
    pub reason: String,
    pub span: (u32, u32, u32, u32),
}

/// Truly unresolvable code pattern (eval/exec/dynamic-import/cross-object
/// reflection/...). Unlike `RawFanoutRef`, candidates cannot even be
/// enumerated — the analyzer just records "this is a blind spot" so
/// downstream LLM tooling can flag the location for manual inspection.
///
/// Carries `file_path` directly (unlike other Raw* types whose file is
/// implicit in their owning `LocalGraph`) because blind spots are
/// passed through to graph-level metadata where the source file must
/// remain identifiable after the LocalGraph is consumed.
#[derive(Debug, Clone)]
pub struct BlindSpot {
    pub kind: String,
    pub file_path: PathBuf,
    pub span: (u32, u32, u32, u32),
    pub hint: String,
}

#[derive(Debug, Clone)]
pub struct LocalGraph {
    pub file_path: PathBuf,
    pub content_hash: [u8; 32],
    pub nodes: Vec<RawNode>,
    pub documents: Vec<RawDocumentBlock>,
    pub imports: Vec<RawImport>,
    pub routes: Vec<RawRoute>,
    pub framework_refs: Vec<RawFrameworkRef>,
    pub fanout_refs: Vec<RawFanoutRef>,
    pub blind_spots: Vec<BlindSpot>,
}
