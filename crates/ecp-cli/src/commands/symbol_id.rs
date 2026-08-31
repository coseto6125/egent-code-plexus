//! FQN (fully-qualified-name) helpers and name→node resolution for
//! `ecp inspect`, `ecp impact` and `ecp path`.

use crate::commands::format::{kind_to_str, node_kind_to_str};
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::session::OverlayView;

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

/// Every merged-space index whose symbol matches `name`, paired with the count
/// of same-named definitions seen BEFORE `--kind` / `--file` / FQN narrowing
/// (the Tier-3 resolver-defence counter, which keys on the global name
/// collision rather than on whichever single def the caller disambiguated to).
///
/// `name` takes the bare symbol or the `Owner.Method` FQN form. `kind_needle`
/// matches the node kind case-insensitively; `file_needle` is a substring of
/// the file path.
///
/// Shared by `ecp impact` and `ecp path` so both land on the same node for the
/// same argument: a path whose endpoints disagree with impact's target would
/// be worse than no path at all.
pub fn resolve_candidates(
    graph: &ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    name: &str,
    kind_needle: Option<&str>,
    file_needle: Option<&str>,
) -> (Vec<usize>, usize) {
    let (owner_filter, bare_name) = split_fqn_target(name);
    let kind_needle = kind_needle.map(|s| s.to_ascii_lowercase());

    let mut same_name_defs = 0usize;
    let mut matches: Vec<usize> = Vec::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        if node.name.resolve(&graph.string_pool) != bare_name {
            continue;
        }
        // Synthetic nodes (e.g. resolver-miss `Annotation` from
        // `decorates_edges`) carry SYNTHETIC_FILE_IDX — they aren't
        // real symbols at any file:line. Drop them from impact targets.
        if !node.has_owning_file() {
            continue;
        }
        // Working-tree truth: a dirty-file symbol deleted/renamed on disk
        // (suppressed by the overlay view) is not a valid impact target, and
        // deliberately stops counting toward `same_name_defs` — the ambiguity
        // caveat describes the on-disk world, not the stale base graph.
        if view.is_some_and(|v| v.redirect(idx as u32).is_none()) {
            continue;
        }
        same_name_defs += 1;
        if let Some(ref kn) = kind_needle {
            let node_kind = kind_to_str(&node.kind).to_ascii_lowercase();
            if &node_kind != kn {
                continue;
            }
        }
        if let Some(needle) = file_needle {
            let file_path = graph.files[node.file_idx.to_native() as usize]
                .path
                .resolve(&graph.string_pool);
            if !file_path.contains(needle) {
                continue;
            }
        }
        if let Some(owner) = owner_filter {
            if !resolve_owner_class(graph, idx)
                .map(|oc| oc == owner)
                .unwrap_or(false)
            {
                continue;
            }
        }
        // A replaced base node enters the merged space as its virtual twin
        // (on-disk spans; masked stale adjacency handled by run_bfs).
        matches.push(match view {
            Some(v) => v.redirect(idx as u32).expect("suppressed filtered above") as usize,
            None => idx,
        });
    }
    // Symbols that only exist in the working tree (new functions in dirty
    // files) — base-replacing twins are excluded: they arrived via redirect.
    if let Some(v) = view {
        for (i, vn) in v.virtual_nodes().iter().enumerate() {
            if vn.replaced_base.is_some() || vn.name != bare_name {
                continue;
            }
            same_name_defs += 1;
            if let Some(ref kn) = kind_needle {
                if &node_kind_to_str(&vn.kind).to_ascii_lowercase() != kn {
                    continue;
                }
            }
            if let Some(needle) = file_needle {
                if !vn.rel_path.contains(needle) {
                    continue;
                }
            }
            if let Some(owner) = owner_filter {
                if vn.owner_class.as_deref() != Some(owner) {
                    continue;
                }
            }
            matches.push(v.base_len() as usize + i);
        }
    }
    (matches, same_name_defs)
}
