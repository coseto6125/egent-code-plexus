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
            .is_some_and(|&idx| archived_is_test(graph, idx as usize)),
    }
}

/// Classify a single symbol given its upstream callers (BFS result).
///
/// `bfs_results` is the slice returned by `run_bfs`; depth-0 entry is the
/// symbol itself. Only depth > 0 entries (actual callers) are examined.
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
    let (det_results, _heur, _hidden_conf, _hidden_heur, _hidden_tests) = run_bfs(
        graph,
        view,
        symbol_idx,
        &Direction::Up,
        depth,
        min_conf,
        include_tests,
        rel_filter,
        false, // include_heuristic
        None,  // max_results — coverage needs the exhaustive walk
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
    let virtual_idx = virtual_uid_index(view);
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
}
