use crate::graph::RelType;
use crate::pool::ArchivedStrRef;

#[derive(Debug, Clone)]
pub enum Value<'arch> {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value<'arch>>),
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
    /// Zero-alloc view over an archived decorator/annotation slice.
    /// Lifetime tied to graph mmap. FU-2026-05-23-006.
    ArchivedStrList {
        items: &'arch [ArchivedStrRef],
        pool: &'arch [u8],
    },
}

impl<'a> PartialEq for Value<'a> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (
                Value::NodeRef {
                    idx: ai,
                    name: an,
                    kind: ak,
                    file_path: af,
                },
                Value::NodeRef {
                    idx: bi,
                    name: bn,
                    kind: bk,
                    file_path: bf,
                },
            ) => ai == bi && an == bn && ak == bk && af == bf,
            (
                Value::EdgeRef {
                    src: as_,
                    tgt: at,
                    rel_type: ar,
                    confidence: ac,
                    reason: asr,
                },
                Value::EdgeRef {
                    src: bs,
                    tgt: bt,
                    rel_type: br,
                    confidence: bc,
                    reason: bsr,
                },
            ) => as_ == bs && at == bt && ar == br && ac == bc && asr == bsr,
            // ArchivedStrList: compare by materializing strings.
            (
                Value::ArchivedStrList {
                    items: ai,
                    pool: ap,
                },
                Value::ArchivedStrList {
                    items: bi,
                    pool: bp,
                },
            ) => {
                if ai.len() != bi.len() {
                    return false;
                }
                ai.iter()
                    .zip(bi.iter())
                    .all(|(a, b)| a.resolve(ap) == b.resolve(bp))
            }
            (Value::ArchivedStrList { items, pool }, Value::List(other))
            | (Value::List(other), Value::ArchivedStrList { items, pool }) => {
                if items.len() != other.len() {
                    return false;
                }
                items.iter().zip(other.iter()).all(|(d, v)| {
                    let s = d.resolve(pool);
                    let ns = s.strip_prefix('@').unwrap_or(s);
                    matches!(v, Value::Str(sv) if sv == ns)
                })
            }
            _ => false,
        }
    }
}

impl<'arch> Value<'arch> {
    /// Convert a transient `Value<'arch>` into an owned `Value<'static>`,
    /// materializing any borrowed slices. Called at executor escape points
    /// (Binding.computed, Accumulator, QueryResult.rows).
    pub fn into_owned(self) -> Value<'static> {
        match self {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(b),
            Value::Int(i) => Value::Int(i),
            Value::Float(f) => Value::Float(f),
            Value::Str(s) => Value::Str(s),
            Value::List(xs) => Value::List(xs.into_iter().map(Value::into_owned).collect()),
            Value::NodeRef {
                idx,
                name,
                kind,
                file_path,
            } => Value::NodeRef {
                idx,
                name,
                kind,
                file_path,
            },
            Value::EdgeRef {
                src,
                tgt,
                rel_type,
                confidence,
                reason,
            } => Value::EdgeRef {
                src,
                tgt,
                rel_type,
                confidence,
                reason,
            },
            Value::ArchivedStrList { items, pool } => Value::List(
                items
                    .iter()
                    .map(|d| {
                        let s = d.resolve(pool);
                        Value::Str(s.strip_prefix('@').unwrap_or(s).to_string())
                    })
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value<'static>>>,
}
