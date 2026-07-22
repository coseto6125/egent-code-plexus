use super::bfs::{merged_node_meta, run_bfs};
use super::Direction;
use ecp_core::session::OverlayView;
use serde_json::{json, Value};

// ── Test-coverage gap analysis ────────────────────────────────────────────────

/// Classification of a symbol's test coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CoverageClass {
    /// Callers exist in prod but zero test callers.
    Uncovered,
    /// test_caller_count >= 1, but prod callers outnumber tests by > 3:1.
    Partial,
    /// test_caller_count >= 1 and prod:test ratio <= 3:1.
    Covered,
    /// No callers at all (entry-point / dead code path).
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
    prod_caller_count: usize,
    class: CoverageClass,
}

/// Check whether a caller node is a test using FunctionMeta.is_test().
///
/// The archived `function_metas` vec is sorted by `node_idx`, so we use
/// binary search. On the archived type, `flags` is `ArchivedU16` and requires
/// `.to_native()` before bitwise ops.
fn archived_is_test(graph: &ecp_core::graph::ArchivedZeroCopyGraph, node_idx: usize) -> bool {
    use ecp_core::graph::FunctionMeta;
    let target = node_idx as u32;
    match graph
        .function_metas
        .binary_search_by_key(&target, |m| m.node_idx.to_native())
    {
        Ok(i) => graph.function_metas[i].flags.to_native() & FunctionMeta::FLAG_TEST != 0,
        Err(_) => false,
    }
}

/// Classify a single symbol given its upstream callers (BFS result).
///
/// `bfs_results` is the slice returned by `run_bfs`; depth-0 entry is the
/// symbol itself. Only depth > 0 entries (actual callers) are examined.
///
/// `uid_idx` is the pre-built `uid → node_idx` table from
/// [`ecp_core::graph_query::build_uid_index`]. Building it once per
/// coverage analysis and passing it here avoids an O(N) linear scan over
/// all graph nodes for every BFS caller entry (T1-6 fast-path).
fn classify_symbol(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    symbol_idx: usize,
    bfs_results: &[Value],
    uid_idx: &rustc_hash::FxHashMap<u64, u32>,
) -> SymbolCoverage {
    let meta = merged_node_meta(graph, view, symbol_idx);
    let uid = meta.uid.to_string();
    let name = meta.name;
    let file = meta.file_path;
    let line = meta.line;
    let kind = meta.kind.to_string();

    let mut test_callers: Vec<String> = Vec::new();
    let mut prod_caller_count: usize = 0;

    for entry in bfs_results
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
    {
        // O(1) uid → node_idx via pre-built FxHashMap (T1-6 fast-path).
        // BFS JSON stores uid as a decimal string; parse back to u64 for lookup.
        let caller_uid = entry["uid"].as_str().unwrap_or("");
        let caller_idx = caller_uid
            .parse::<u64>()
            .ok()
            .and_then(|u| uid_idx.get(&u).map(|&i| i as usize));

        let is_test = caller_idx
            .map(|idx| archived_is_test(graph, idx))
            .unwrap_or(false);

        if is_test {
            let caller_name = entry["name"].as_str().unwrap_or(caller_uid).to_string();
            test_callers.push(caller_name);
        } else {
            prod_caller_count += 1;
        }
    }

    let test_caller_count = test_callers.len();
    let class = match (test_caller_count, prod_caller_count) {
        (0, 0) => CoverageClass::Orphan,
        (0, _) => CoverageClass::Uncovered,
        (t, p) if p > t * 3 => CoverageClass::Partial,
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
        prod_caller_count,
        class,
    }
}

#[allow(clippy::too_many_arguments)]
fn coverage_bfs_for_symbol(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    symbol_idx: usize,
    requested_direction: &Direction,
    existing_bfs: &[Value],
    depth: usize,
    min_conf: f32,
    include_tests: bool,
    rel_filter: &Option<Vec<String>>,
) -> Vec<Value> {
    if *requested_direction == Direction::Up {
        return existing_bfs.to_vec();
    }
    // Coverage analysis only consumes deterministic upstream callers — discard
    // the heuristic / hidden-count fields from #264's expanded run_bfs return.
    let (det_results, _heur, _hidden_conf, _hidden_heur) = run_bfs(
        graph,
        view,
        symbol_idx,
        &Direction::Up,
        depth,
        min_conf,
        include_tests,
        rel_filter,
        false, // include_heuristic
    );
    det_results
}

#[allow(clippy::too_many_arguments)]
pub(super) fn coverage_analyses(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    bfs_by_symbol: &[(usize, Vec<Value>)],
    requested_direction: &Direction,
    depth: usize,
    min_conf: f32,
    include_tests: bool,
    rel_filter: &Option<Vec<String>>,
) -> Vec<SymbolCoverage> {
    // Build uid → node_idx once for the whole analysis. classify_symbol
    // needs to reverse-look-up caller uids (from BFS JSON) back to node
    // indices to call archived_is_test; without this table each caller
    // entry would do an O(N) linear scan. (T1-6 fast-path)
    let uid_idx = ecp_core::graph_query::build_uid_index(graph);
    bfs_by_symbol
        .iter()
        .map(|(idx, bfs)| {
            let coverage_bfs = coverage_bfs_for_symbol(
                graph,
                view,
                *idx,
                requested_direction,
                bfs,
                depth,
                min_conf,
                include_tests,
                rel_filter,
            );
            classify_symbol(graph, view, *idx, &coverage_bfs, &uid_idx)
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
