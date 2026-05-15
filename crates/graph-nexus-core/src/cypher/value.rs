use crate::graph::RelType;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    /// Reference to a graph node. CLI side resolves `.name`/`.kind`/`.filePath`
    /// for human-readable serialization.
    NodeRef {
        idx: u32,
        name: String,
        kind: String,
        file_path: String,
    },
    EdgeRef {
        src: u32,
        tgt: u32,
        rel_type: RelType,
        confidence: f32,
        reason: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}
