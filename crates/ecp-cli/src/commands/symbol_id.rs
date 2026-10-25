//! FQN (fully-qualified-name) helpers, the Tier-3 collision count, and the
//! name→node resolution `ecp path` still uses. `ecp impact` and `ecp inspect`
//! resolve through [`crate::read_context::resolve_target`].

use crate::commands::format::{kind_to_str, node_kind_to_str};
use ecp_core::contracts::identity::normalize_file_needle;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::session::{MergedGraph, OverlayView};

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
pub fn split_fqn_target(s: &str) -> (Option<&str>, &str) {
    match s.rsplit_once('.') {
        Some((owner, name)) => (Some(owner), name),
        None => (None, s),
    }
}

fn owner_matches(actual: Option<&str>, requested: &str, file: &str) -> bool {
    actual.is_some_and(|owner| {
        owner == requested
            || (file.ends_with(".rs")
                && !requested.contains("::")
                && owner.rsplit("::").next() == Some(requested))
    })
}

/// The Tier-3 resolver-defence counter of `bare_name`: the same-named
/// definitions the index weighed, which key on the global name collision
/// rather than on whichever single def the caller disambiguated to. It is the
/// larger of the on-disk world (surviving base defs plus working-tree-only
/// defs) and the indexed world (every base def, including those the overlay
/// suppressed): the index decided whether to drop bare calls from the indexed
/// collision, which an uncommitted rename must not hide. A later overload is
/// never bound by name, so it is no collision.
pub fn same_name_def_count(
    graph: &ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    bare_name: &str,
) -> usize {
    let mut base_defs = 0usize;
    let mut disk_defs = 0usize;
    for idx in graph.name_candidates(bare_name) {
        let node = &graph.nodes[idx as usize];
        if node.name.resolve(&graph.string_pool) != bare_name
            || !node.has_owning_file()
            || graph.is_later_overload(idx)
        {
            continue;
        }
        base_defs += 1;
        if view.is_none_or(|v| v.redirect(idx).is_some()) {
            disk_defs += 1;
        }
    }
    if let Some(v) = view {
        disk_defs += v
            .virtual_nodes()
            .iter()
            .filter(|vn| vn.replaced_base.is_none() && !vn.later_overload && vn.name == bare_name)
            .count();
    }
    base_defs.max(disk_defs)
}

/// Every merged-space index whose symbol matches `name`, paired with
/// [`same_name_def_count`].
///
/// `name` takes the bare symbol or the `Owner.Method` FQN form. The owner
/// compares with the display owner ([`MergedGraph::display_owner`], the text
/// `inspect` prints and the read context's target resolver matches), so
/// `outer.helper` names a Rust fn nested in `outer`, whose stored owner is
/// empty, in the index and in a dirty file alike. `kind_needle`
/// matches the node kind case-insensitively; `file_needle` is a substring of
/// the file path. When a substring leaves several matches, a path equal to the
/// needle wins, then paths ending with `/needle` (see `narrow_to_path_suffix`),
/// so `cmd/main.go` can pick itself over `tools/cmd/main.go`.
///
/// No command resolves its target here any more: `ecp path`, `ecp impact`
/// and `ecp inspect` use [`crate::read_context::resolve_target`].
pub fn resolve_candidates(
    graph: &ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    name: &str,
    kind_needle: Option<&str>,
    file_needle: Option<&str>,
) -> (Vec<usize>, usize) {
    let (owner_filter, bare_name) = split_fqn_target(name);
    let kind_needle = kind_needle.map(|s| s.to_ascii_lowercase());
    // Graph paths are `/`-separated and repo-relative; a Windows-style or `./`
    // needle would otherwise match nothing.
    let file_needle = file_needle.map(normalize_file_needle);
    let file_needle = file_needle.as_deref();

    let merged = MergedGraph::new(graph, view);
    let mut matches: Vec<(usize, &str)> = Vec::new();
    // Ascending like the full scan, so the candidate list keeps node order.
    for idx in graph.name_candidates(bare_name) {
        let (idx, node) = (idx as usize, &graph.nodes[idx as usize]);
        if node.name.resolve(&graph.string_pool) != bare_name {
            continue;
        }
        // Synthetic nodes (e.g. resolver-miss `Annotation` from
        // `decorates_edges`) carry SYNTHETIC_FILE_IDX — they aren't
        // real symbols at any file:line. Drop them from impact targets.
        // A later overload is reached through the first one (calls bind to
        // the first, which references it), and the resolver never sees it
        // by name, so it is neither a target nor a collision.
        if !node.has_owning_file() || graph.is_later_overload(idx as u32) {
            continue;
        }
        // Working-tree truth: a dirty-file symbol deleted/renamed on disk
        // (suppressed by the overlay view) is not a valid impact target. A
        // replaced base node enters the merged space as its virtual twin
        // (on-disk spans; masked stale adjacency handled by run_bfs).
        let Some(at) = view.map_or(Some(idx as u32), |v| v.redirect(idx as u32)) else {
            continue;
        };
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
        let file = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool);
        if let Some(owner) = owner_filter {
            if !owner_matches(merged.display_owner(at).as_deref(), owner, file) {
                continue;
            }
        }
        matches.push((at as usize, file));
    }
    // Symbols that only exist in the working tree (new functions in dirty
    // files) — base-replacing twins are excluded: they arrived via redirect.
    if let Some(v) = view {
        for (at, vn) in (v.base_len()..).zip(v.virtual_nodes()) {
            if vn.replaced_base.is_some() || vn.later_overload || vn.name != bare_name {
                continue;
            }
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
                if !owner_matches(merged.display_owner(at).as_deref(), owner, &vn.rel_path) {
                    continue;
                }
            }
            matches.push((at as usize, &*vn.rel_path));
        }
    }
    if let Some(needle) = file_needle {
        narrow_to_path_suffix(&mut matches, needle);
    }
    (
        matches.into_iter().map(|(idx, _)| idx).collect(),
        same_name_def_count(graph, view, bare_name),
    )
}

/// Every substring of a shorter path is a substring of a longer one that
/// contains it, so `--file cmd/main.go` alone cannot choose `cmd/main.go` over
/// `tools/cmd/main.go`. Keep the matches whose path is the needle or ends with
/// `/needle`. Keep the substring set when none does (a directory or partial
/// needle) or when the needle is empty.
fn narrow_to_path_suffix(matches: &mut Vec<(usize, &str)>, needle: &str) {
    let needle = needle.trim_end_matches('/');
    if matches.len() < 2 || needle.is_empty() {
        return;
    }
    // Equality before a `/`-bounded suffix: `cmd/main.go` is also a bounded
    // suffix of `tools/cmd/main.go`, so only equality can pick the shorter one.
    let equal = |path: &str| path == needle;
    let bounded = |path: &str| {
        path.strip_suffix(needle)
            .is_some_and(|head| head.ends_with('/'))
    };
    for tier in [&equal as &dyn Fn(&str) -> bool, &bounded] {
        if matches.iter().any(|&(_, path)| tier(path)) {
            matches.retain(|&(_, path)| tier(path));
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::graph::{NodeKind, ZeroCopyGraph};
    use ecp_core::graph_fixture::GraphFixture;
    use ecp_core::session::{OverlayFileInput, OverlaySymbol};

    /// `source` parsed as `src/lib.rs` and built as the index builds it.
    fn rust_graph_bytes(source: &str) -> rkyv::util::AlignedVec {
        use ecp_analyzer::resolution::builder::GraphBuilder;
        let mut builder = GraphBuilder::new();
        builder.add_graph(parse_rust(source));
        rkyv::to_bytes::<rkyv::rancor::Error>(&builder.build()).unwrap()
    }

    fn parse_rust(source: &str) -> ecp_core::analyzer::types::LocalGraph {
        use ecp_analyzer::rust::parser::RustProvider;
        use ecp_core::analyzer::provider::LanguageProvider;
        RustProvider::new()
            .unwrap()
            .parse_file(std::path::Path::new("src/lib.rs"), source.as_bytes())
            .unwrap()
    }

    /// The read context's answer for `text`, as a candidate count: 1 for a
    /// unique target, the total for an ambiguity, 0 for none.
    fn resolve_target_count(merged: &MergedGraph<'_>, text: &str) -> usize {
        use ecp_core::contracts::identity::TargetResolution;
        let request = crate::read_context::target_request(text, None, None).unwrap();
        match crate::read_context::resolve_target(merged, &request) {
            TargetResolution::Unique(_) => 1,
            TargetResolution::Ambiguous(a) => a.total(),
            TargetResolution::NotFound { .. } => 0,
        }
    }

    /// Stands for: a fn in a nested module is named by its innermost module
    /// (R6 stores the owner `b`, not `a::b`), in the index and in a dirty
    /// copy, and the owner filter agrees with the read context's resolver
    /// on every form. C1 "Qualifier match": the request segments equal a
    /// segment-bounded suffix of the qualified path, here `[b]`, so `a::b.f`
    /// names nothing. Before R6 the stored owner was `a::b` and both forms
    /// matched.
    #[test]
    fn test_resolve_candidates_rust_nested_module_short_owner_matches_base_and_overlay() {
        let bytes = rust_graph_bytes("mod a { pub mod b { pub fn f() {} } }");
        let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let count = |view: Option<&OverlayView>, text: &str| {
            resolve_candidates(archived, view, text, None, None).0.len()
        };
        assert_eq!(count(None, "b.f"), 1);
        let merged = MergedGraph::new(archived, None);
        for text in ["f", "b.f", "a::b.f", "other::b.f"] {
            assert_eq!(
                count(None, text),
                resolve_target_count(&merged, text),
                "{text}"
            );
        }

        let dirty: OverlayFileInput =
            parse_rust("\nmod a { pub mod b { pub fn f() {} pub fn added() {} } }").into();
        let view = OverlayView::build(archived, &[dirty]).unwrap();
        let merged = MergedGraph::new(archived, Some(&view));
        for name in ["f", "added"] {
            assert_eq!(count(Some(&view), &format!("b.{name}")), 1, "{name}");
            for text in [format!("b.{name}"), format!("a::b.{name}")] {
                assert_eq!(
                    count(Some(&view), &text),
                    resolve_target_count(&merged, &text),
                    "{text}"
                );
            }
        }
    }

    /// Stands for: the owner filter reads the display owner, so `outer.helper`
    /// names the Rust fn nested in `outer` (stored owner empty, I1) and not
    /// the file-level `helper`, in the index, in a dirty copy, and for a
    /// nested fn the edit adds. Before the fix it read the stored owner and
    /// matched nothing.
    #[test]
    fn test_resolve_candidates_rust_nested_fn_display_owner_selects_nested() {
        let source = "fn outer() {\n    fn helper() {}\n    helper();\n}\nfn helper() {}\n";
        let bytes = rust_graph_bytes(source);
        let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let line_of =
            |merged: &MergedGraph<'_>, idx: usize| merged.node(idx as u32).map(|n| n.start_line());
        let base = MergedGraph::new(archived, None);
        let nested = resolve_candidates(archived, None, "outer.helper", None, None).0;
        assert_eq!(nested.len(), 1, "{nested:?}");
        assert_eq!(line_of(&base, nested[0]), Some(2));
        assert_eq!(
            resolve_candidates(archived, None, "helper", None, None)
                .0
                .len(),
            2
        );
        // A blank qualifier names no owner, so it selects neither `helper`.
        for text in ["other.helper", " .helper", "\t.helper"] {
            assert!(
                resolve_candidates(archived, None, text, None, None)
                    .0
                    .is_empty(),
                "{text:?}"
            );
        }

        let edited = format!(
            "\n{}",
            source.replace(
                "fn helper() {}\n    helper();",
                "fn helper() {}\n    fn added() {}\n    helper();"
            )
        );
        let view = OverlayView::build(archived, &[parse_rust(&edited).into()]).unwrap();
        let merged = MergedGraph::new(archived, Some(&view));
        for (name, line) in [("helper", 3), ("added", 4)] {
            let found =
                resolve_candidates(archived, Some(&view), &format!("outer.{name}"), None, None).0;
            assert_eq!(found.len(), 1, "outer.{name}: {found:?}");
            assert!(
                found[0] as u32 >= view.base_len(),
                "outer.{name} is a virtual node"
            );
            assert_eq!(line_of(&merged, found[0]), Some(line), "outer.{name}");
        }
    }

    /// "dup" repeats across kinds, owners and files; "shared" has 1200 nodes
    /// (see `same_name_nodes`). Also a tombstone (empty name) and a unicode
    /// name. Every node has its own uid.
    fn same_name_graph() -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        fx.same_name_nodes(
            "shared",
            1200,
            |i| format!("src/s{i}.ts"),
            |fx, i, path, _| {
                if i % 100 == 0 {
                    fx.func(path, "");
                    fx.func(path, "naïve_函数");
                    fx.func(path, "dup");
                    fx.method("src/b.ts", &format!("Owner{i}"), "dup");
                    fx.method(path, "Owner", "dup");
                }
            },
        );
        fx.build()
    }

    fn resolve_matches(
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
                export_aliases: Vec::new(),
                rel_path: "src/b.ts".to_string(),
                symbols: vec![
                    OverlaySymbol {
                        is_exported: false,
                        name: "dup".to_string(),
                        kind: NodeKind::Function,
                        owner_class: None,
                        start_line: 1,
                        end_line: 2,
                        start_column: 0,
                        end_column: 0,
                        calls: vec![],
                        heritage: Vec::new(),
                        variant_refs: Vec::new(),
                        return_type: None,
                        generic_arity: 0,
                        flags: None,
                    },
                    OverlaySymbol {
                        is_exported: false,
                        name: "brand_new".to_string(),
                        kind: NodeKind::Function,
                        owner_class: None,
                        start_line: 3,
                        end_line: 4,
                        start_column: 0,
                        end_column: 0,
                        calls: vec![],
                        heritage: Vec::new(),
                        variant_refs: Vec::new(),
                        return_type: None,
                        generic_arity: 0,
                        flags: None,
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
                resolve_matches(fast, name, kind, file, overlay),
                resolve_matches(slow, name, kind, file, overlay),
                "{name:?} kind={kind:?} file={file:?} overlay={overlay}"
            );
        }
    }

    fn matched_paths(g: ZeroCopyGraph, name: &str, file: Option<&str>) -> Vec<String> {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        resolve_candidates(graph, None, name, None, file)
            .0
            .into_iter()
            .map(|idx| {
                graph.files[graph.nodes[idx].file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.string_pool)
                    .to_string()
            })
            .collect()
    }

    fn nested_path_pair_graph() -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        for path in [
            "cmd/main.go",
            "tools/cmd/main.go",
            "src/index.ts",
            "packages/a/src/index.ts",
        ] {
            fx.func(path, "main");
        }
        fx.build()
    }

    /// Contract: `--file` naming the shorter of two nested paths selects it,
    /// instead of failing as ambiguous because the longer path contains it.
    #[test]
    fn test_resolve_candidates_file_needle_prefers_path_suffix_over_substring() {
        for (needle, want) in [
            ("cmd/main.go", vec!["cmd/main.go"]),
            ("tools/cmd/main.go", vec!["tools/cmd/main.go"]),
            ("src/index.ts", vec!["src/index.ts"]),
            ("packages/a/src/index.ts", vec!["packages/a/src/index.ts"]),
            ("./cmd/main.go", vec!["cmd/main.go"]),
            ("cmd\\main.go", vec!["cmd/main.go"]),
            ("main.go", vec!["cmd/main.go", "tools/cmd/main.go"]),
        ] {
            assert_eq!(
                matched_paths(nested_path_pair_graph(), "main", Some(needle)),
                want,
                "needle {needle:?}"
            );
        }
    }

    /// A needle that is no path suffix (a directory, a fragment) keeps the
    /// substring set, so the narrowing never turns a hit into a miss.
    #[test]
    fn test_resolve_candidates_file_needle_without_suffix_match_keeps_substring_set() {
        for needle in ["cmd/", "cmd", "ools/cmd", "/"] {
            assert_eq!(
                matched_paths(nested_path_pair_graph(), "main", Some(needle))
                    .iter()
                    .filter(|p| p.ends_with("main.go"))
                    .count(),
                if needle == "ools/cmd" { 1 } else { 2 },
                "needle {needle:?}"
            );
        }
        assert!(matched_paths(nested_path_pair_graph(), "main", Some("nope.go")).is_empty());
    }

    #[test]
    fn test_resolve_candidates_shared_name_lists_every_node_ascending() {
        let (matches, same_name_defs) =
            resolve_matches(same_name_graph(), "shared", None, None, false);
        assert_eq!(matches.len(), 1200);
        assert_eq!(same_name_defs, 1200);
        assert!(matches.windows(2).all(|w| w[0] < w[1]), "node order");
    }

    /// The tombstone has an empty name: only the scan matches it, so an
    /// empty bare name (`ecp impact --target Owner.`) must keep the scan.
    #[test]
    fn test_resolve_candidates_empty_bare_name_keeps_tombstone_matches() {
        let (matches, same_name_defs) = resolve_matches(same_name_graph(), "", None, None, false);
        assert_eq!(matches.len(), 12);
        assert_eq!(same_name_defs, 12);
    }

    /// A base node the overlay replaced or suppressed is not a candidate, but
    /// still counts toward `same_name_defs` (the index saw that collision);
    /// the overlay-only symbol is a candidate.
    #[test]
    fn test_resolve_candidates_overlay_drops_suppressed_base_nodes() {
        let (plain, plain_defs) = resolve_matches(same_name_graph(), "dup", None, None, false);
        let (merged, merged_defs) = resolve_matches(same_name_graph(), "dup", None, None, true);
        assert_eq!(plain_defs, 36);
        // base 36, disk 36 - 12 (src/b.ts) + 1 (overlay `dup`) = 25; max, not sum.
        assert_eq!(merged_defs, 36, "plain {plain_defs}");
        assert!(merged.len() < plain.len());
        let (new_only, _) = resolve_matches(same_name_graph(), "brand_new", None, None, true);
        assert_eq!(new_only.len(), 1);
    }
}
