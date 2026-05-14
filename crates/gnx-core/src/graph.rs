use crate::pool::StrRef;
use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
pub enum NodeKind {
    File,
    Function,
    Class,
    Method,
    Interface,
    Constructor,
    Property,
    Variable,
    Const,
    Import,
    Route,
    Process,
    Document,
    Section,
}

impl NodeKind {
    /// True when the node represents an invokable target (CALLS edge sink).
    pub const fn is_callable(self) -> bool {
        matches!(self, Self::Function | Self::Method | Self::Constructor)
    }

    /// True when the node represents an extendable / type-binding target
    /// (EXTENDS edges, type annotations).
    pub const fn is_type(self) -> bool {
        matches!(self, Self::Class | Self::Interface)
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
pub enum RelType {
    Defines,
    Imports,
    Calls,
    Extends,
    Implements,
    HasMethod,
    HasProperty,
    Accesses,
    HandlesRoute,
    StepInProcess,
    References,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct Node {
    pub uid: StrRef,
    pub name: StrRef,
    pub file_idx: u32,
    pub kind: NodeKind,
    pub span: (u32, u32, u32, u32), // start_line, start_col, end_line, end_col
    pub community_id: u16,          // 0 = unassigned
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct Edge {
    pub source: u32,
    pub target: u32,
    pub rel_type: RelType,
    pub confidence: f32,
    pub reason: StrRef,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
pub enum FileCategory {
    Source,
    Test,
    Document,
    Config,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct File {
    pub path: StrRef,
    pub mtime: u64,
    pub content_hash: [u8; 32],
    pub category: FileCategory,
}

#[derive(Archive, Deserialize, Serialize, Debug)]
#[rkyv(derive(Debug))]
pub struct ZeroCopyGraph {
    pub magic: [u8; 8],
    pub fingerprint: [u8; 32],
    pub string_pool: Vec<u8>,
    pub files: Vec<File>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub out_offsets: Vec<u32>,
    pub in_offsets: Vec<u32>,
    pub in_edge_idx: Vec<u32>,
    pub name_index: Vec<u32>,
    pub embeddings: Option<Vec<Vec<f32>>>,

    /// Boundary index: `nodes[process_start..]` are all `NodeKind::Process`.
    /// For node_idx >= process_start, `process_k = node_idx - process_start`
    /// and its trace lives in `traces_data[traces_offsets[k]..traces_offsets[k+1]]`.
    pub process_start: u32,
    pub traces_offsets: Vec<u32>,
    pub traces_data: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::StringPool;
    use rkyv::rancor::Error;

    #[test]
    fn test_serialize_deserialize_graph() {
        let mut pool = StringPool::new();
        let name_ref = pool.add("main");
        let uid_ref = pool.add("Function:src/main.ts:main");

        let graph = ZeroCopyGraph {
            magic: *b"GNX-RS\0\0",
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: name_ref.clone(),
                mtime: 0,
                content_hash: [0; 32],
                category: FileCategory::Source,
            }],
            nodes: vec![Node {
                uid: uid_ref,
                name: name_ref,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (1, 0, 5, 0),
                community_id: 0,
            }],
            edges: vec![],
            out_offsets: vec![0, 0],
            in_offsets: vec![0, 0],
            in_edge_idx: vec![],
            name_index: vec![],
            embeddings: None,
            process_start: 1,
            traces_offsets: vec![],
            traces_data: vec![],
        };

        // Serialize
        let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();

        // Deserialize / Zero-copy access
        let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&bytes).unwrap();

        assert_eq!(archived.magic, *b"GNX-RS\0\0");
        assert_eq!(archived.nodes.len(), 1);

        // Resolve string using the archived string pool
        let archived_node = &archived.nodes[0];
        let name_str = archived_node.name.resolve(&archived.string_pool);
        assert_eq!(name_str, "main");
    }
}
