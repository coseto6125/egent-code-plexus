//! FQN (fully-qualified-name) helpers and name→node resolution for
//! `ecp inspect`, `ecp impact` and `ecp path`.

use crate::commands::format::{kind_to_str, node_kind_to_str};
use ecp_core::graph::{ArchivedNode, ArchivedZeroCopyGraph};
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
    // Ascending like the full scan, so the candidate list keeps node order.
    let named = graph.nodes_named_sorted(bare_name);
    let scanned: Box<dyn Iterator<Item = (usize, &ArchivedNode)> + '_> = match &named {
        Some(hits) => Box::new(hits.iter().map(|&i| (i as usize, &graph.nodes[i as usize]))),
        None => Box::new(graph.nodes.iter().enumerate()),
    };
    for (idx, node) in scanned {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::graph::{NodeKind, ZeroCopyGraph};
    use ecp_core::graph_fixture::GraphFixture;
    use ecp_core::session::{OverlayFileInput, OverlaySymbol};

    /// "dup" repeats across kinds, owners and files; "shared" has 1200 nodes
    /// so the index's hash-only unstable sort leaves them out of node order.
    /// Also a tombstone (empty name) and a unicode name. Every node has its
    /// own uid.
    fn same_name_graph() -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        for i in 0..1200 {
            let path = format!("src/s{i}.ts");
            match i % 3 {
                0 => fx.func(&path, "shared"),
                1 => fx.method(&path, "Owner", "shared"),
                _ => fx.node(NodeKind::Class, &path, "shared"),
            };
            if i % 100 == 0 {
                fx.func(&path, "");
                fx.func(&path, "naïve_函数");
                fx.func(&path, "dup");
                fx.method("src/b.ts", &format!("Owner{i}"), "dup");
                fx.method(&path, "Owner", "dup");
            }
        }
        fx.build()
    }

    fn resolve(
        g: ZeroCopyGraph,
        name: &str,
        kind: Option<&str>,
        file: Option<&str>,
        overlay: bool,
    ) -> (Vec<usize>, usize) {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let view = overlay.then(|| {
            let dirty = OverlayFileInput {
                rel_path: "src/b.ts".to_string(),
                symbols: vec![
                    OverlaySymbol {
                        name: "dup".to_string(),
                        kind: NodeKind::Function,
                        owner_class: None,
                        start_line: 1,
                        end_line: 2,
                        calls: vec![],
                    },
                    OverlaySymbol {
                        name: "brand_new".to_string(),
                        kind: NodeKind::Function,
                        owner_class: None,
                        start_line: 3,
                        end_line: 4,
                        calls: vec![],
                    },
                ],
                imports: vec![],
            };
            OverlayView::build(graph, &[dirty]).unwrap()
        });
        resolve_candidates(graph, view.as_ref(), name, kind, file)
    }

    /// Contract: the name index changes time only. Same candidates in the
    /// same order, and the same `same_name_defs`, as the full scan that
    /// clearing `name_index` forces. The order is what `ecp impact` prints
    /// in its ambiguity list.
    #[test]
    fn test_resolve_candidates_with_index_matches_full_scan() {
        let cases: [(&str, Option<&str>, Option<&str>, bool); 14] = [
            ("shared", None, None, false),
            ("shared", Some("Method"), None, false),
            ("shared", Some("class"), Some("s3"), false),
            ("Owner.shared", None, None, false),
            ("Nobody.shared", None, None, false),
            ("dup", None, None, false),
            ("dup", None, None, true),
            ("Owner.dup", None, None, true),
            ("dup", Some("function"), Some("src/"), true),
            ("brand_new", None, None, true),
            ("naïve_函数", None, None, false),
            ("absent", None, None, false),
            ("Owner.", None, None, false),
            ("", None, None, false),
        ];
        for (name, kind, file, overlay) in cases {
            let fast = same_name_graph();
            let mut slow = same_name_graph();
            slow.name_index.clear();
            assert!(!fast.name_index.is_empty());
            assert_eq!(
                resolve(fast, name, kind, file, overlay),
                resolve(slow, name, kind, file, overlay),
                "{name:?} kind={kind:?} file={file:?} overlay={overlay}"
            );
        }
    }

    #[test]
    fn test_resolve_candidates_shared_name_lists_every_node_ascending() {
        let (matches, same_name_defs) = resolve(same_name_graph(), "shared", None, None, false);
        assert_eq!(matches.len(), 1200);
        assert_eq!(same_name_defs, 1200);
        assert!(matches.windows(2).all(|w| w[0] < w[1]), "node order");
    }

    /// The tombstone has an empty name: only the scan matches it, so an
    /// empty bare name (`ecp impact --target Owner.`) must keep the scan.
    #[test]
    fn test_resolve_candidates_empty_bare_name_keeps_tombstone_matches() {
        let (matches, same_name_defs) = resolve(same_name_graph(), "", None, None, false);
        assert_eq!(matches.len(), 12);
        assert_eq!(same_name_defs, 12);
    }

    /// A base node the overlay replaced or suppressed no longer counts and is
    /// not a candidate; the overlay-only symbol still is.
    #[test]
    fn test_resolve_candidates_overlay_drops_suppressed_base_nodes() {
        let (plain, plain_defs) = resolve(same_name_graph(), "dup", None, None, false);
        let (merged, merged_defs) = resolve(same_name_graph(), "dup", None, None, true);
        assert_eq!(plain_defs, 36);
        assert!(merged_defs < plain_defs, "{merged_defs} vs {plain_defs}");
        assert!(merged.len() < plain.len());
        let (new_only, _) = resolve(same_name_graph(), "brand_new", None, None, true);
        assert_eq!(new_only.len(), 1);
    }
}
