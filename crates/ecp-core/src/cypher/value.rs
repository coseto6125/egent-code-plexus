use crate::graph::RelType;
use compact_str::CompactString;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// `CompactString` inlines values ≤24 bytes with no heap allocation. The
    /// dominant result strings — symbol `kind` ("Function", "Class"…) and
    /// `rel_type` ("Calls", "Implements"…) — always fit, so a query returning
    /// N edges drops N heap allocs for those columns.
    Str(CompactString),
    List(Vec<Value>),
    /// Reference to a graph node. CLI side resolves `.name`/`.kind`/`.filePath`
    /// for human-readable serialization. `name`/`kind` inline as `CompactString`
    /// (kind always; realistic symbol names usually); `file_path` stays `String`
    /// since paths routinely exceed the inline budget.
    NodeRef {
        idx: u32,
        name: CompactString,
        kind: CompactString,
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
        self.write_key(buf, false);
    }

    /// Aggregate keys retain edge reasons, matching their original Debug keys.
    /// Row DISTINCT/UNION keep their existing reason-independent identity.
    pub(super) fn write_aggregate_key(&self, buf: &mut Vec<u8>) {
        self.write_key(buf, true);
    }

    /// Key of a node by identity; the same bytes as a `NodeRef` with that index.
    pub(super) fn write_node_key(idx: u32, buf: &mut Vec<u8>) {
        buf.push(6);
        buf.extend_from_slice(&idx.to_le_bytes());
    }

    fn write_key(&self, buf: &mut Vec<u8>, include_edge_reason: bool) {
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
                    item.write_key(buf, include_edge_reason);
                }
            }
            Value::NodeRef { idx, .. } => Self::write_node_key(*idx, buf),
            Value::EdgeRef {
                src,
                tgt,
                rel_type,
                confidence,
                reason,
            } => {
                buf.push(7);
                buf.extend_from_slice(&src.to_le_bytes());
                buf.extend_from_slice(&tgt.to_le_bytes());
                buf.push(*rel_type as u8);
                buf.extend_from_slice(&confidence.to_bits().to_le_bytes());
                if include_edge_reason {
                    buf.extend_from_slice(&(reason.len() as u32).to_le_bytes());
                    buf.extend_from_slice(reason.as_bytes());
                }
            }
        }
    }
}

/// An edge confidence as the decimal the indexer wrote. The graph stores f32,
/// and widening 0.7f32 gives 0.699999988, which `r.confidence < 0.7` matches
/// and `= 0.7` misses. The shortest decimal (at most 9 places) that converts
/// back to the same f32 is that decimal, and it keeps every distinct f32
/// distinct, so `0.7000004` stays above 0.7. Past 9 places it widens as is.
pub fn confidence_f64(confidence: f32) -> f64 {
    let wide = f64::from(confidence);
    let mut scale = 1.0;
    for _ in 0..9 {
        scale *= 10.0;
        let decimal = (wide * scale).round() / scale;
        if decimal as f32 == confidence {
            return decimal;
        }
    }
    wide
}

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}
