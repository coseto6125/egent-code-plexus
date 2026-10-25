//! FQN (fully-qualified-name) helpers and the Tier-3 collision count. Every
//! command resolves its target through
//! [`crate::read_context::resolve_target`].

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

/// The Tier-3 resolver-defence counter of `bare_name`: the same-named
/// definitions the index weighed, which key on the global name collision
/// rather than on whichever single def the caller disambiguated to. It is the
/// larger of the on-disk world (surviving base defs plus working-tree-only
/// defs) and the indexed world (every base def, including those the overlay
/// suppressed): the index decided whether to drop bare calls from the indexed
/// collision, which an uncommitted rename must not hide. A later overload is
/// never bound by name, and a lexically scoped def is never a Tier-3
/// candidate (invariant I5), so neither is a collision.
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
            || graph.is_lexically_scoped(idx)
        {
            continue;
        }
        base_defs += 1;
        if view.is_none_or(|v| v.redirect(idx).is_some()) {
            disk_defs += 1;
        }
    }
    if let Some(v) = view {
        disk_defs += (v.base_len()..)
            .zip(v.virtual_nodes())
            .filter(|&(idx, vn)| {
                vn.replaced_base.is_none()
                    && !vn.later_overload
                    && vn.name == bare_name
                    && v.lexical_parent(idx).is_none()
            })
            .count();
    }
    base_defs.max(disk_defs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::session::{MergedGraph, OverlayFileInput};

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

    /// Stands for: the Tier-3 collision counter skips a lexically scoped
    /// namesake, in the index and in a dirty copy, since the ladder never
    /// weighs it; a caveat built on it would send the agent after callers
    /// that cannot exist.
    #[test]
    fn test_same_name_def_count_scoped_namesake_not_counted_base_and_overlay() {
        let source = "fn helper() {}\nfn run() { fn helper() {} helper(); }\n";
        let bytes = rust_graph_bytes(source);
        let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(same_name_def_count(archived, None, "helper"), 1);

        let dirty: OverlayFileInput = parse_rust(&format!("\n{source}")).into();
        let view = OverlayView::build(archived, &[dirty]).unwrap();
        assert_eq!(same_name_def_count(archived, Some(&view), "helper"), 1);
        assert_eq!(same_name_def_count(archived, Some(&view), "absent"), 0);
    }

    /// Stands for: a fn in a nested module is named by its innermost module
    /// (R6 stores the owner `b`, not `a::b`), in the index and in a dirty
    /// copy. C1 "Qualifier match": the request segments equal a
    /// segment-bounded suffix of the qualified path, here `[b]`, so `a::b.f`
    /// names nothing (FU-2026-10-26-94c43bd8a2ca). Before R6 the stored owner
    /// was `a::b` and both forms matched.
    #[test]
    fn test_resolve_target_rust_nested_module_short_owner_matches_base_and_overlay() {
        let bytes = rust_graph_bytes("mod a { pub mod b { pub fn f() {} } }");
        let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let merged = MergedGraph::new(archived, None);
        for (text, want) in [("f", 1), ("b.f", 1), ("a::b.f", 0), ("other::b.f", 0)] {
            assert_eq!(resolve_target_count(&merged, text), want, "{text}");
        }

        let dirty: OverlayFileInput =
            parse_rust("\nmod a { pub mod b { pub fn f() {} pub fn added() {} } }").into();
        let view = OverlayView::build(archived, &[dirty]).unwrap();
        let merged = MergedGraph::new(archived, Some(&view));
        for name in ["f", "added"] {
            assert_eq!(
                resolve_target_count(&merged, &format!("b.{name}")),
                1,
                "{name}"
            );
        }
    }
}
