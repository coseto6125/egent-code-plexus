//! Deterministic u64 UID for graph nodes.
//!
//! Canonical byte order (why `\0` separators): `\0` cannot appear in any
//! valid Rust identifier or POSIX path component, so no two distinct
//! `(kind, path, owner_class, name)` tuples can produce the same byte stream.

use xxhash_rust::xxh3::Xxh3;

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
