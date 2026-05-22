//! FQN (fully-qualified-name) helpers for `ecp inspect` and `ecp impact`.

use ecp_core::graph::ArchivedZeroCopyGraph;

/// Resolve the owner class name for a node by reading `Node.owner_class`
/// directly (added in T1-4 / PR #285). O(1) field read.
///
/// Returns the owning class name when set, `None` for module-level symbols
/// (StrRef::default with len=0 — empty string resolves to "").
pub fn resolve_owner_class(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> Option<&str> {
    let oc = graph.nodes[node_idx]
        .owner_class
        .resolve(&graph.string_pool);
    if oc.is_empty() {
        None
    } else {
        Some(oc)
    }
}

/// Format a fully-qualified name from an optional owner class and a bare name.
///
/// - `Some("Foo"), "validate"` → `"Foo.validate"`
/// - `None, "validate"` → `"validate"`
pub fn format_fqn(owner: Option<&str>, name: &str) -> String {
    match owner {
        Some(o) if !o.is_empty() => format!("{o}.{name}"),
        _ => name.to_owned(),
    }
}

/// Parse a `--name` / `--target` argument into an optional owner prefix and
/// bare symbol name.
///
/// - `"Foo.validate"` → `(Some("Foo"), "validate")`
/// - `"pkg.Foo.validate"` → `(Some("pkg.Foo"), "validate")`
/// - `"validate"` → `(None, "validate")`
///
/// Splits on the **last** `.` so the bare name on the right matches
/// `Node.name` (which is always a bare identifier) while everything left of
/// the final dot becomes the owner prefix. This admits namespaced owners
/// (`pkg.Foo`) without changing the single-level (`Foo.validate`) contract.
///
/// `rename` (PR #285) currently splits on the first `.` — that PR will be
/// migrated to share this helper as a follow-up to keep dot semantics
/// uniform across the CLI.
pub fn split_fqn_target(s: &str) -> (Option<&str>, &str) {
    match s.rsplit_once('.') {
        Some((owner, name)) => (Some(owner), name),
        None => (None, s),
    }
}
