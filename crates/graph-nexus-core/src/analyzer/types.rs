use crate::graph::NodeKind;
use rkyv::{Archive, Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct IdentifierRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub row: usize,
    pub col: usize,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
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

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum BindingKind {
    /// Symbol → symbol (typedef, identifier-bodied `#define`, extern decls).
    Alias,
    /// Symbol → literal value (`#define MAX 4096`, `#define VER "v1"`).
    Constant,
    /// Symbol → expression (function-like `#define ADD(a,b)`, parenthesized expressions).
    Macro,
    /// Empty body, non-guard (`#define DEBUG`, `#define ENABLE_FOO`).
    Flag,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawImport {
    pub source: String,
    pub imported_name: String,
    pub alias: Option<String>,
    /// `None` for ordinary import statements; `Some(_)` for C named bindings
    /// (`typedef`, `#define`, `extern`) classified by body shape.
    pub binding_kind: Option<BindingKind>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawRoute {
    pub method: String,
    pub path: String,
    pub handler: Option<String>,
    pub span: (u32, u32, u32, u32),
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawDocumentBlock {
    pub name: String,
    pub is_section: bool,
    pub span: (u32, u32, u32, u32),
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
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
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
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
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct BlindSpot {
    pub kind: String,
    #[rkyv(with = rkyv::with::AsString)]
    pub file_path: PathBuf,
    pub span: (u32, u32, u32, u32),
    pub hint: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct LocalGraph {
    #[rkyv(with = rkyv::with::AsString)]
    pub file_path: PathBuf,
    pub content_hash: [u8; 8],
    pub nodes: Vec<RawNode>,
    pub documents: Vec<RawDocumentBlock>,
    pub imports: Vec<RawImport>,
    pub routes: Vec<RawRoute>,
    pub framework_refs: Vec<RawFrameworkRef>,
    pub fanout_refs: Vec<RawFanoutRef>,
    pub blind_spots: Vec<BlindSpot>,
}
