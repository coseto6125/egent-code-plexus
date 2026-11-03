use super::bfs::{base_meta_is_test, merged_node_meta, run_bfs, WalkOpts, REQUIRES_VERIFICATION};
use super::Direction;
use ecp_core::session::OverlayView;
use serde_json::{json, Value};
use std::borrow::Cow;

// ── Test-coverage gap analysis ────────────────────────────────────────────────

/// Classification of a symbol's test coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CoverageClass {
    /// Callers exist (prod, or tests behind an edge to verify), but no
    /// verified (untagged) test caller.
    Uncovered,
    /// test_caller_count >= 1, but prod callers outnumber tests by > 3:1.
    Partial,
    /// test_caller_count >= 1 and prod:test ratio <= 3:1.
    Covered,
    /// No callers at all, not even an unverified test caller (entry-point /
    /// dead code path).
    Orphan,
}

/// Data collected for a single symbol during coverage analysis.
pub(super) struct SymbolCoverage {
    uid: String,
    name: String,
    file: String,
    line: u32,
    kind: String,
    test_callers: Vec<String>,
    test_caller_count: usize,
    /// Test callers behind a heuristic or sub-0.7 edge: reported, never
    /// counted as coverage.
    unverified_test_callers: usize,
    prod_caller_count: usize,
    class: CoverageClass,
}

/// uid → merged index of the overlay's nodes, the first node of a uid
/// winning. Empty without an overlay.
fn virtual_uid_index(view: Option<&OverlayView>) -> rustc_hash::FxHashMap<u64, u32> {
    let mut index = rustc_hash::FxHashMap::default();
    if let Some(view) = view {
        for (idx, node) in (view.base_len()..).zip(view.virtual_nodes()) {
            index.entry(node.uid).or_insert(idx);
        }
    }
    index
}

/// Whether the caller with `uid` is a test by its `FunctionMeta` flags. A
/// working-tree node (`virtual_idx`) reads the flags its fragment paired
/// ([`OverlayView::function_meta`]), as the cypher executor does: a reindex
/// stores those, never its base twin's, and a new node has no base twin.
fn is_test_caller(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    uid_idx: &rustc_hash::FxHashMap<u64, u32>,
    virtual_idx: &rustc_hash::FxHashMap<u64, u32>,
    uid: u64,
) -> bool {
    use ecp_core::graph::FunctionMeta;
    match (view, virtual_idx.get(&uid)) {
        (Some(view), Some(&idx)) => view
            .function_meta(idx)
            .is_some_and(|meta| meta.flags & FunctionMeta::FLAG_TEST != 0),
        _ => uid_idx
            .get(&uid)
            .is_some_and(|&idx| base_meta_is_test(graph, idx as usize)),
    }
}

/// Classify a single symbol given its upstream callers (BFS result).
///
/// A test caller tagged `requires_verification` (a heuristic or sub-0.7 edge)
/// does not make the symbol covered: it is counted apart, as
/// `unverified_test_callers`.
///
/// `bfs_results` holds both buckets `run_bfs` returns; the depth-0 entry is
/// the symbol itself. Only depth > 0 entries (actual callers) are examined.
///
/// `uid_idx` is the pre-built `uid → node_idx` table from
/// [`ecp_core::graph_query::build_uid_index`], and `virtual_idx` the same
/// table over the overlay's nodes. Building them once per coverage
/// analysis and passing them here avoids an O(N) linear scan over all
/// graph nodes for every BFS caller entry (T1-6 fast-path).
fn classify_symbol(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    symbol_idx: usize,
    bfs_results: &[Value],
    uid_idx: &rustc_hash::FxHashMap<u64, u32>,
    virtual_idx: &rustc_hash::FxHashMap<u64, u32>,
) -> SymbolCoverage {
    let meta = merged_node_meta(graph, view, symbol_idx);
    let uid = meta.uid.to_string();
    let name = meta.name;
    let file = meta.file_path;
    let line = meta.line;
    let kind = meta.kind.to_string();

    let mut test_callers: Vec<String> = Vec::new();
    let mut unverified_test_callers: usize = 0;
    let mut prod_caller_count: usize = 0;

    for entry in bfs_results
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
    {
        // O(1) uid → node_idx via pre-built FxHashMap (T1-6 fast-path).
        // BFS JSON stores uid as a decimal string; parse back to u64 for lookup.
        let caller_uid = entry["uid"].as_str().unwrap_or("");
        let is_test = caller_uid
            .parse::<u64>()
            .is_ok_and(|uid| is_test_caller(graph, view, uid_idx, virtual_idx, uid));

        if is_test && entry[REQUIRES_VERIFICATION].as_bool().unwrap_or(false) {
            unverified_test_callers += 1;
        } else if is_test {
            let caller_name = entry["name"].as_str().unwrap_or(caller_uid).to_string();
            test_callers.push(caller_name);
        } else {
            prod_caller_count += 1;
        }
    }

    let test_caller_count = test_callers.len();
    let class = match (
        test_caller_count,
        prod_caller_count,
        unverified_test_callers,
    ) {
        (0, 0, 0) => CoverageClass::Orphan,
        (0, _, _) => CoverageClass::Uncovered,
        (t, p, _) if p > t * 3 => CoverageClass::Partial,
        _ => CoverageClass::Covered,
    };

    SymbolCoverage {
        uid,
        name,
        file,
        line,
        kind,
        test_callers,
        test_caller_count,
        unverified_test_callers,
        prod_caller_count,
        class,
    }
}

/// The upstream walk of `symbol_idx` under `opts`: `existing_bfs` itself when
/// `opts` already walks upstream, else a rerun upstream.
fn coverage_bfs_for_symbol<'b>(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    symbol_idx: usize,
    existing_bfs: &'b [Value],
    opts: &WalkOpts<'_>,
) -> Cow<'b, [Value]> {
    if opts.direction == Direction::Up {
        return Cow::Borrowed(existing_bfs);
    }
    // Both buckets, under the requested heuristic policy: a test caller
    // reached through a heuristic edge counts as `unverified_test_callers`
    // here as in an upstream request, rather than vanishing.
    let (mut results, heuristic, _hidden_conf, _hidden_heur, _hidden_tests) = run_bfs(
        graph,
        view,
        symbol_idx,
        &Direction::Up,
        opts.depth,
        opts.min_conf,
        opts.include_tests,
        opts.rel_filter,
        opts.include_heuristic,
        None, // max_results — coverage needs the exhaustive walk
    );
    results.extend(heuristic);
    Cow::Owned(results)
}

pub(super) fn coverage_analyses(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    bfs_by_symbol: &[(usize, Vec<Value>)],
    opts: &WalkOpts<'_>,
) -> Vec<SymbolCoverage> {
    // Build uid → node_idx once for the whole analysis. classify_symbol
    // needs to reverse-look-up caller uids (from BFS JSON) back to node
    // indices to call base_meta_is_test; without this table each caller
    // entry would do an O(N) linear scan. (T1-6 fast-path)
    let uid_idx = ecp_core::graph_query::build_uid_index(graph);
    let virtual_idx = virtual_uid_index(view);
    bfs_by_symbol
        .iter()
        .map(|(idx, bfs)| {
            let coverage_bfs = coverage_bfs_for_symbol(graph, view, *idx, bfs, opts);
            classify_symbol(graph, view, *idx, &coverage_bfs, &uid_idx, &virtual_idx)
        })
        .collect()
}

/// Build the `coverage` JSON section from a list of per-symbol analyses.
pub(super) fn build_coverage_json(analyses: Vec<SymbolCoverage>) -> Value {
    let mut uncovered: Vec<Value> = Vec::new();
    let mut partial: Vec<Value> = Vec::new();
    let mut covered: Vec<Value> = Vec::new();
    let mut orphans: Vec<Value> = Vec::new();

    for s in analyses {
        let base = json!({
            "uid": s.uid,
            "name": s.name,
            "file": s.file,
            "line": s.line,
            "kind": s.kind,
            "test_caller_count": s.test_caller_count,
            "unverified_test_callers": s.unverified_test_callers,
            "prod_caller_count": s.prod_caller_count,
        });
        match s.class {
            CoverageClass::Uncovered => uncovered.push(base),
            CoverageClass::Partial => {
                let mut v = base;
                v["tests"] = json!(s.test_callers);
                partial.push(v);
            }
            CoverageClass::Covered => {
                let mut v = base;
                v["tests"] = json!(s.test_callers);
                covered.push(v);
            }
            CoverageClass::Orphan => orphans.push(base),
        }
    }

    let total_analyzed = uncovered.len() + partial.len() + covered.len() + orphans.len();
    json!({
        "summary": {
            "uncovered": uncovered.len(),
            "partial": partial.len(),
            "covered": covered.len(),
            "orphan": orphans.len(),
            "total_analyzed": total_analyzed,
        },
        "uncovered_symbols": uncovered,
        "partial_symbols": partial,
        "covered_symbols": covered,
        "orphan_symbols": orphans,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::graph::{ArchivedZeroCopyGraph, FunctionMeta, NodeKind};
    use ecp_core::graph_fixture::GraphFixture;
    use ecp_core::session::view::{OverlayFileInput, OverlaySymbol};

    const DIRTY: &str = "src/dirty.rs";

    fn function(name: &str, lines: (u32, u32), flags: u16) -> OverlaySymbol {
        OverlaySymbol {
            is_exported: false,
            name: name.to_string(),
            kind: NodeKind::Function,
            owner_class: None,
            start_line: lines.0,
            end_line: lines.1,
            start_column: 0,
            end_column: 0,
            calls: vec!["target".to_string()],
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: Some(flags),
            decorators: Vec::new(),
            params: Vec::new(),
        }
    }

    fn caller(uid: u64, name: &str) -> Value {
        json!({ "uid": uid.to_string(), "name": name, "depth": 1 })
    }

    /// Base nodes: 0 `target` (src/lib.rs) · 1 `checks` (dirty), archived
    /// with no test flag · 2 `was_test` (dirty), archived as a test. The
    /// dirty file now marks `checks` a test, adds the test `fresh` (no base
    /// twin), and unmarks `was_test`. Each caller reads its fragment's
    /// flags, as a reindex stores them; without the overlay each reads the
    /// archived ones.
    #[test]
    fn test_classify_symbol_dirty_callers_read_fragment_test_flags() {
        let mut fx = GraphFixture::new();
        let target = fx.func("src/lib.rs", "target");
        let checks = fx.func(DIRTY, "checks");
        let was_test = fx.func(DIRTY, "was_test");
        fx.function_meta(checks, 0, &[]);
        fx.function_meta(was_test, FunctionMeta::FLAG_TEST, &[]);
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let input = OverlayFileInput {
            parsed_language: None,
            rel_path: DIRTY.to_string(),
            symbols: vec![
                function("checks", (1, 3), FunctionMeta::FLAG_TEST),
                function("fresh", (5, 7), FunctionMeta::FLAG_TEST),
                function("was_test", (9, 11), 0),
            ],
            imports: Vec::new(),
            export_aliases: Vec::new(),
        };
        let view = OverlayView::build(graph, std::slice::from_ref(&input)).expect("a dirty file");
        let base_len = view.base_len();
        let uid_of = |offset: u32| view.node(base_len + offset).expect("virtual node").uid;
        let callers = [
            caller(uid_of(0), "checks"),
            caller(uid_of(1), "fresh"),
            caller(uid_of(2), "was_test"),
        ];
        assert_eq!(uid_of(0), graph.nodes[checks as usize].uid.to_native());
        let uid_idx = ecp_core::graph_query::build_uid_index(graph);

        let overlay = classify_symbol(
            graph,
            Some(&view),
            target as usize,
            &callers,
            &uid_idx,
            &virtual_uid_index(Some(&view)),
        );
        assert_eq!(overlay.test_callers, ["checks", "fresh"]);
        assert_eq!(overlay.prod_caller_count, 1);

        let base = classify_symbol(
            graph,
            None,
            target as usize,
            &[callers[0].clone(), callers[2].clone()],
            &uid_idx,
            &virtual_uid_index(None),
        );
        assert_eq!(base.test_callers, ["was_test"]);
        assert_eq!(base.prod_caller_count, 1);
    }

    fn tagged(mut entry: Value, requires_verification: bool) -> Value {
        entry[REQUIRES_VERIFICATION] = json!(requires_verification);
        entry
    }

    /// Stands for: a test caller behind a sub-0.7 edge is no coverage. It is
    /// counted as `unverified_test_callers`, so a symbol whose only test edge
    /// is a 0.55 guess classifies as `Uncovered` (before: `Covered`, with the
    /// guess listed in `tests`). A tagged prod caller still counts as prod.
    #[test]
    fn test_classify_symbol_tagged_test_callers_are_not_coverage() {
        let mut fx = GraphFixture::new();
        let target = fx.func("src/lib.rs", "target");
        let sure = fx.func("tests/a.rs", "sure");
        let guess = fx.func("tests/b.rs", "guess");
        let prod = fx.func("src/p.rs", "prod");
        fx.function_meta(sure, FunctionMeta::FLAG_TEST, &[]);
        fx.function_meta(guess, FunctionMeta::FLAG_TEST, &[]);
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let uid = |idx: u32| graph.nodes[idx as usize].uid.to_native();
        let uid_idx = ecp_core::graph_query::build_uid_index(graph);
        let classify = |callers: &[Value]| {
            classify_symbol(
                graph,
                None,
                target as usize,
                callers,
                &uid_idx,
                &virtual_uid_index(None),
            )
        };

        let guess_only = classify(&[
            tagged(caller(uid(guess), "guess"), true),
            tagged(caller(uid(prod), "prod"), true),
        ]);
        assert!(guess_only.test_callers.is_empty());
        assert_eq!(guess_only.test_caller_count, 0);
        assert_eq!(guess_only.unverified_test_callers, 1);
        assert_eq!(guess_only.prod_caller_count, 1);
        assert_eq!(guess_only.class, CoverageClass::Uncovered);

        let mixed = classify(&[
            tagged(caller(uid(sure), "sure"), false),
            tagged(caller(uid(guess), "guess"), true),
            caller(uid(prod), "prod"),
        ]);
        assert_eq!(mixed.test_callers, ["sure"]);
        assert_eq!(mixed.unverified_test_callers, 1);
        assert_eq!(mixed.class, CoverageClass::Covered);

        let json = build_coverage_json(vec![guess_only, mixed]);
        assert_eq!(json["uncovered_symbols"][0]["unverified_test_callers"], 1);
        assert_eq!(json["covered_symbols"][0]["unverified_test_callers"], 1);
        assert_eq!(json["covered_symbols"][0]["tests"], json!(["sure"]));
    }

    /// Stands for: a symbol whose only callers are tagged test callers has
    /// callers, so it is `Uncovered`, not `Orphan` (dead code). Before the fix
    /// the `(0 tests, 0 prod)` match made it an orphan; no caller at all
    /// stays an orphan.
    #[test]
    fn test_classify_symbol_only_tagged_test_caller_is_uncovered_not_orphan() {
        let mut fx = GraphFixture::new();
        let target = fx.func("src/lib.rs", "target");
        let guess = fx.func("tests/b.rs", "guess");
        fx.function_meta(guess, FunctionMeta::FLAG_TEST, &[]);
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let uid_idx = ecp_core::graph_query::build_uid_index(graph);
        let classify = |callers: &[Value]| {
            classify_symbol(
                graph,
                None,
                target as usize,
                callers,
                &uid_idx,
                &virtual_uid_index(None),
            )
        };
        let guess_uid = graph.nodes[guess as usize].uid.to_native();

        let tagged_only = classify(&[tagged(caller(guess_uid, "guess"), true)]);
        assert_eq!(tagged_only.unverified_test_callers, 1);
        assert_eq!(tagged_only.class, CoverageClass::Uncovered);
        assert_eq!(classify(&[]).class, CoverageClass::Orphan);
    }
}
