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
/// [`overload`]) in the declaration's overload set. The uid has no
/// parameter signature, so `save(int)` and `save(String)` share one. A
/// kind outside this set keeps its repeat as a node of its own too, as a
/// redefinition, unless it [`merges_repeats`].
pub fn keeps_overloads(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Method | NodeKind::Constructor)
}

/// Whether a later declaration that repeats an earlier one's identity
/// fields is the same entity, merged into the earlier one and kept as a
/// tombstone: one field assigned in two methods, a JS `var x` twice, a
/// Python `x = 1; x = 2`. No such kind is a scope container.
pub fn merges_repeats(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Property | NodeKind::Variable | NodeKind::Const
    )
}

/// UID of the `ordinal`-th (1-based) later overload of the declaration whose
/// uid is `first`: deterministic for a given declaration order in the file.
pub fn overload(first: u64, ordinal: u32) -> u64 {
    let mut h = Xxh3::new();
    h.update(&first.to_le_bytes());
    h.update(&ordinal.to_le_bytes());
    h.digest()
}

/// UID of the tombstone that holds the slot of a merged repeat
/// ([`merges_repeats`]): the bit-inverse of `uid`. The caller passes the
/// repeat's own [`overload`] uid, whose ordinal no other declaration of the
/// base uid holds, so the tombstones of one base uid are pairwise distinct
/// and never equal the survivor's uid.
pub fn tombstone(uid: u64) -> u64 {
    !uid
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
