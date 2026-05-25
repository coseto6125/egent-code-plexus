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

impl Value {
    /// Append a self-describing byte key for DISTINCT/UNION dedup into `buf`.
    ///
    /// Replaces `format!("{self:?}")`: no per-row Debug-string allocation,
    /// and the key is collision-free (a leading discriminant tag per variant
    /// plus length-prefixed bytes — so `["a","b"]` cannot alias `["ab"]`, and
    /// `Int(1)` cannot alias `Float(1.0)`). `f64`/`f32` go through `to_bits`
    /// so the key hashes by exact bit pattern.
    pub fn write_dedup_key(&self, buf: &mut Vec<u8>) {
        match self {
            Value::Null => buf.push(0),
            Value::Bool(b) => {
                buf.push(1);
                buf.push(*b as u8);
            }
            Value::Int(i) => {
                buf.push(2);
                buf.extend_from_slice(&i.to_le_bytes());
            }
            Value::Float(f) => {
                buf.push(3);
                buf.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            Value::Str(s) => {
                buf.push(4);
                buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
                buf.extend_from_slice(s.as_bytes());
            }
            Value::List(items) => {
                buf.push(5);
                buf.extend_from_slice(&(items.len() as u32).to_le_bytes());
                for item in items {
                    item.write_dedup_key(buf);
                }
            }
            Value::NodeRef { idx, .. } => {
                buf.push(6);
                buf.extend_from_slice(&idx.to_le_bytes());
            }
            Value::EdgeRef {
                src,
                tgt,
                rel_type,
                confidence,
                ..
            } => {
                buf.push(7);
                buf.extend_from_slice(&src.to_le_bytes());
                buf.extend_from_slice(&tgt.to_le_bytes());
                buf.push(*rel_type as u8);
                buf.extend_from_slice(&confidence.to_bits().to_le_bytes());
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}
