use super::bfs::{merged_node_meta, run_bfs, WalkOpts, REQUIRES_VERIFICATION};
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

/// Classify a single symbol given its upstream callers (BFS result).
///
/// A test caller tagged `requires_verification` (a heuristic or sub-0.7 edge)
/// does not make the symbol covered: it is counted apart, as
/// `unverified_test_callers`.
///
/// `bfs_results` holds both buckets `run_bfs` returns; the depth-0 entry is
/// the symbol itself. Only depth > 0 entries (actual callers) are examined.
///
/// A caller is a test caller when its row is tagged `test: true`, the tag
/// `run_bfs` sets from the file category or the node's test flag. A coverage
/// payload never disagrees with its own rows.
fn classify_symbol(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    symbol_idx: usize,
    bfs_results: &[Value],
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
        let is_test = entry["test"].as_bool().unwrap_or(false);

        if is_test && entry[REQUIRES_VERIFICATION].as_bool().unwrap_or(false) {
            unverified_test_callers += 1;
        } else if is_test {
            let caller_name = entry["name"]
                .as_str()
                .or_else(|| entry["uid"].as_str())
                .unwrap_or("")
                .to_string();
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
    bfs_by_symbol
        .iter()
        .map(|(idx, bfs)| {
            let coverage_bfs = coverage_bfs_for_symbol(graph, view, *idx, bfs, opts);
            classify_symbol(graph, view, *idx, &coverage_bfs)
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
    use ecp_core::graph::ArchivedZeroCopyGraph;
    use ecp_core::graph_fixture::GraphFixture;

    fn caller(uid: u64, name: &str, test: bool) -> Value {
        json!({ "uid": uid.to_string(), "name": name, "depth": 1, "test": test })
    }

    fn tagged(mut entry: Value, requires_verification: bool) -> Value {
        entry[REQUIRES_VERIFICATION] = json!(requires_verification);
        entry
    }

    /// Stands for: a caller is a test caller exactly when its row says
    /// `test: true`. The coverage payload never disagrees with its rows.
    #[test]
    fn test_classify_symbol_reads_row_test_tag_counts_tagged_rows_as_test_callers() {
        let mut fx = GraphFixture::new();
        let target = fx.func("src/lib.rs", "target");
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();

        let result = classify_symbol(
            graph,
            None,
            target as usize,
            &[
                caller(1, "checks", true),
                caller(2, "fresh", true),
                caller(3, "prod", false),
                json!({ "uid": "4", "name": "untagged", "depth": 1 }),
            ],
        );
        assert_eq!(result.test_callers, ["checks", "fresh"]);
        assert_eq!(result.prod_caller_count, 2);
    }

    /// Stands for: a test caller behind a sub-0.7 edge is no coverage. It is
    /// counted as `unverified_test_callers`, so a symbol whose only test edge
    /// is a 0.55 guess classifies as `Uncovered` (before: `Covered`, with the
    /// guess listed in `tests`). A tagged prod caller still counts as prod.
    #[test]
    fn test_classify_symbol_tagged_test_callers_are_not_coverage() {
        let mut fx = GraphFixture::new();
        let target = fx.func("src/lib.rs", "target");
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let classify = |callers: &[Value]| classify_symbol(graph, None, target as usize, callers);

        let guess_only = classify(&[
            tagged(caller(2, "guess", true), true),
            tagged(caller(3, "prod", false), true),
        ]);
        assert!(guess_only.test_callers.is_empty());
        assert_eq!(guess_only.test_caller_count, 0);
        assert_eq!(guess_only.unverified_test_callers, 1);
        assert_eq!(guess_only.prod_caller_count, 1);
        assert_eq!(guess_only.class, CoverageClass::Uncovered);

        let mixed = classify(&[
            tagged(caller(1, "sure", true), false),
            tagged(caller(2, "guess", true), true),
            caller(3, "prod", false),
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
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let classify = |callers: &[Value]| classify_symbol(graph, None, target as usize, callers);

        let tagged_only = classify(&[tagged(caller(2, "guess", true), true)]);
        assert_eq!(tagged_only.unverified_test_callers, 1);
        assert_eq!(tagged_only.class, CoverageClass::Uncovered);
        assert_eq!(classify(&[]).class, CoverageClass::Orphan);
    }
}
