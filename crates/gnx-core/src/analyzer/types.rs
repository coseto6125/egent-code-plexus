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
}

#[derive(Debug, Clone)]
pub struct RawImport {
    pub source: String,
    pub imported_name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LocalGraph {
    pub file_path: PathBuf,
    pub nodes: Vec<RawNode>,
    pub imports: Vec<RawImport>,
}
