//! Deterministic u64 UID for graph nodes.
//!
//! Canonical byte order (why `\0` separators): `\0` cannot appear in any
//! valid Rust identifier or POSIX path component, so no two distinct
//! `(kind, path, owner_class, name)` tuples can produce the same byte stream.

use xxhash_rust::xxh3::{xxh3_64, Xxh3};

use crate::graph::NodeKind;

/// Compute a deterministic xxh3-64 UID from the four node-identity fields.
///
/// Zero heap allocations: each fragment is passed as a separate `update()`
/// slice; no `String`, `Vec`, or `format!` is used.
///
/// Canonical stream: `kind_as_str \0 path \0 owner_class \0 name`
pub fn compute(kind: NodeKind, path: &str, owner_class: Option<&str>, name: &str) -> u64 {
    let mut h = Xxh3::new();
    h.update(kind.as_str().as_bytes());
    h.update(b"\0");
    h.update(path.as_bytes());
    h.update(b"\0");
    h.update(owner_class.unwrap_or("").as_bytes());
    h.update(b"\0");
    h.update(name.as_bytes());
    h.digest()
}

/// Whether a later declaration that repeats an earlier one's identity
/// fields is an overload the index keeps as its own node (uid from
/// [`overload`]), rather than a duplicate it tombstones. The uid has no
/// parameter signature, so `save(int)` and `save(String)` share one.
pub fn keeps_overloads(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Method | NodeKind::Constructor)
}

/// UID of the `ordinal`-th (1-based) later overload of the declaration whose
/// uid is `first`: deterministic for a given declaration order in the file.
pub fn overload(first: u64, ordinal: u32) -> u64 {
    let mut h = Xxh3::new();
    h.update(&first.to_le_bytes());
    h.update(&ordinal.to_le_bytes());
    h.digest()
}

/// One-shot xxh3-64 hash of raw bytes. Used for per-symbol content hashing
/// (T7-2): `content_hash = xxh3_64_bytes(&source[start_byte..end_byte])`.
///
/// Wrapper over `xxhash_rust::xxh3::xxh3_64` exposed here so callers in
/// `ecp_analyzer` obtain the hash via the existing `ecp_core` dependency
/// rather than adding a new `xxhash-rust` dep to `ecp-analyzer`.
#[inline]
pub fn xxh3_64_bytes(bytes: &[u8]) -> u64 {
    xxh3_64(bytes)
}
