use super::bfs::{merged_node_meta, run_bfs};
use super::coverage::{build_coverage_json, coverage_analyses};
use super::payload::SymbolImpactPayload;
use super::{
    parse_csv_lower, resolve_min_conf, ImpactArgs, ImpactHints, DEFAULT_CONFIDENCE_THRESHOLD,
};
use crate::commands::format::{kind_to_str, node_kind_to_str};
use crate::commands::impact::{
    attach_heuristic_fields, attach_hidden_edges, direction_str, Direction,
};
use crate::commands::symbol_id::{format_fqn, resolve_owner_class, split_fqn_target};
use crate::engine::Engine;
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::HashSet;

// ── Per-symbol library API (used by `ecp group impact`) ─────────────────────

/// Result of a single-symbol local impact computation.
///
/// Wraps the JSON payload produced by `impact_by_name` so that callers can
/// extract the symbol UIDs touched by the traversal without re-parsing the
/// full payload themselves.
pub struct LocalImpact {
    payload: Value,
}

impl LocalImpact {
    /// UIDs of every node reached by the BFS (depth 0 = the target itself).
    /// Returns an empty vec when the payload carries an `"error"` field.
    pub fn direct_symbol_uids(&self) -> Vec<&str> {
        self.payload["impact"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v["uid"].as_str()).collect())
            .unwrap_or_default()
    }

    /// Number of nodes in the BFS result (excluding the start node at depth 0).
    pub fn direct_count(&self) -> usize {
        self.payload["impact"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter(|v| v["depth"].as_u64().unwrap_or(0) > 0)
                    .count()
            })
            .unwrap_or(0)
    }

    /// The full JSON payload — same shape as `ecp impact --format json`.
    pub fn as_json(&self) -> &Value {
        &self.payload
    }
}

/// Per-symbol impact computation callable without a CLI context.
///
/// `member_repo` is the `dir_name` or alias of the indexed repo; it is used
/// only to resolve the confidence threshold from the repo config — the Engine
/// is provided by the caller, so no graph loading happens here.
///
/// `file` narrows the target to one path (substring match, as `--file` does).
/// Callers that know which file the symbol lives in should pass it: a bare name
/// with several definitions is rejected as `AmbiguousSymbol`, and the common
/// names (`run`, `new`, `handle`) are precisely the ambiguous ones.
///
/// Returns `Ok(LocalImpact)` even when the symbol is not found in the graph
/// (the payload will carry an `"error"` field in that case), matching the
/// same graceful-degradation behaviour as `ecp impact --target X`.
#[allow(clippy::too_many_arguments)]
pub fn run_for_symbol(
    engine: &Engine,
    member_repo: &str,
    target: &str,
    file: Option<&str>,
    direction: &str,
    max_depth: Option<u32>,
    timeout_ms: Option<u64>,
    include_tests: bool,
    max_results: Option<usize>,
) -> Result<LocalImpact, EcpError> {
    let dir = match direction.to_ascii_lowercase().as_str() {
        "downstream" | "down" => Direction::Down,
        "both" => Direction::Both,
        _ => Direction::Up,
    };
    let args = ImpactArgs {
        name: Some(target.to_string()),
        target: None,
        baseline: None,
        file: file.map(str::to_string),
        kind: None,
        direction: dir,
        depth: max_depth.unwrap_or(5) as usize,
        high_trust_only: false,
        min_confidence: None,
        include_tests,
        relation_types: None,
        repo: Some(member_repo.to_string()),
        test_coverage: false,
        format: None,
        no_heuristic: true,
        confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
        explain_confidence: false,
        literal: None,
        literal_coherence: false,
        batch: false,
        max_results,
    };
    let _ = timeout_ms; // timeout enforcement is caller-side; passed for API parity
    let (payload, _hints) = super::build_payload_with_hints(&args, engine)?;
    Ok(LocalImpact { payload })
}

pub(super) fn impact_by_name(
    args: &ImpactArgs,
    engine: &Engine,
) -> Result<(Value, ImpactHints), EcpError> {
    let name = args
        .name
        .as_deref()
        .or(args.target.as_deref())
        .expect("build_payload_with_hints gates on name||target");
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let view = engine.overlay_view();

    // Split `Owner.Method` form for precise targeting.
    let (owner_filter, bare_name) = split_fqn_target(name);

    // Resolve name → matching node indices, with optional --file / --kind
    // disambiguation. FQN `Owner.Method` form is an additional filter on top.
    let file_needle = args.file.as_deref();
    let kind_needle = args.kind.as_deref().map(|s| s.to_ascii_lowercase());

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
        // Counted BEFORE --kind/--file/FQN narrowing: the Tier-3 resolver
        // defence keys on the global name collision, not on whichever
        // single def the user disambiguated to.
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

    if matches.is_empty() {
        return Err(EcpError::InvalidArgument(format!(
            "No symbol named '{}' found in graph. Try `ecp find <name> --mode fuzzy` to find candidates, or check --file / --kind filters",
            format_fqn(owner_filter, bare_name)
        )));
    }

    // Multiple matches without disambiguation → report candidates then fail.
    // FQN targeting (owner_filter present) already narrows by owner, so only
    // fall into the ambiguous branch when the remaining options still exceed 1.
    if matches.len() > 1 && file_needle.is_none() && kind_needle.is_none() {
        let fqn_label = format_fqn(owner_filter, bare_name);
        let candidates: Vec<Value> = matches
            .iter()
            .map(|&i| {
                let meta = merged_node_meta(graph, view, i);
                json!({
                    "kind": meta.kind,
                    "filePath": meta.file_path,
                    "line": meta.line,
                })
            })
            .collect();
        let candidate_lines = candidates
            .iter()
            .map(|candidate| {
                format!(
                    "  {},{},{}",
                    candidate["filePath"].as_str().unwrap_or(""),
                    candidate["kind"].as_str().unwrap_or(""),
                    candidate["line"].as_u64().unwrap_or(0)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(EcpError::AmbiguousSymbol {
            name: fqn_label.to_string(),
            count: matches.len(),
            candidates: Some(candidate_lines),
        });
    }

    let min_conf = resolve_min_conf(args);
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    // --test-coverage implies --include-tests so test callers are reachable.
    let effective_include_tests = args.include_tests || args.test_coverage;

    let mut all_results: Vec<Value> = Vec::new();
    let mut all_heuristic_results: Vec<Value> = Vec::new();
    let mut hidden_edges_total: u64 = 0;
    let mut hidden_heuristic_total: u64 = 0;
    let mut per_match_bfs: Vec<(usize, Vec<Value>)> = Vec::new();
    for start_idx in &matches {
        // A budget across the WHOLE call, not per match: a name with k
        // definitions would otherwise materialise k x max_results nodes.
        let remaining = args
            .max_results
            .map(|cap| cap.saturating_sub(all_results.len()));
        if remaining == Some(0) {
            break;
        }
        let (det_results, heur_results, hidden_conf, hidden_heur) = run_bfs(
            graph,
            view,
            *start_idx,
            &args.direction,
            args.depth,
            min_conf,
            effective_include_tests,
            &rel_filter,
            !args.no_heuristic,
            remaining,
        );
        all_results.extend(det_results.iter().cloned());
        per_match_bfs.push((*start_idx, det_results));
        all_heuristic_results.extend(heur_results);
        hidden_edges_total += hidden_conf;
        hidden_heuristic_total += hidden_heur;
    }

    // Empty callers hint for upstream direction.
    let impact_without_start: Vec<&Value> = all_results
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
        .collect();
    let emit_empty_hint = impact_without_start.is_empty() && args.direction == Direction::Up;
    // A field target with no readers: the hint must flag that some languages
    // don't model field reads yet, so empty != provably unread.
    let empty_hint_is_field = emit_empty_hint
        && all_results
            .iter()
            .any(|e| e["depth"].as_u64() == Some(0) && e["kind"].as_str() == Some("property"));

    // Collect unique file paths across ALL matches so the blind-spot warning
    // is accurate when --file / --kind still leaves >1 match.
    let mut seen_files = HashSet::new();
    let target_file_paths: Vec<String> = matches
        .iter()
        .map(|&idx| merged_node_meta(graph, view, idx).file_path)
        .filter(|p| seen_files.insert(p.clone()))
        .collect();

    let mut all_blind_spot_kinds: Vec<String> = Vec::new();
    for fp in &target_file_paths {
        all_blind_spot_kinds.extend(collect_blind_spots(graph, fp));
    }

    // Use the original user-supplied name (which may be FQN) as the target
    // label in output — more precise than bare_name when owner was specified.
    let payload = SymbolImpactPayload {
        status: "success".to_string(),
        target: format_fqn(owner_filter, bare_name),
        direction: direction_str(&args.direction).to_string(),
        impact: all_results,
    };
    let mut result_obj =
        serde_json::to_value(&payload).map_err(|e| EcpError::Serialization(e.to_string()))?;
    attach_hidden_edges(&mut result_obj, hidden_edges_total);
    attach_heuristic_fields(
        &mut result_obj,
        hidden_heuristic_total,
        all_heuristic_results,
        !args.no_heuristic,
        args.explain_confidence,
        args.confidence_threshold,
    );

    if !all_blind_spot_kinds.is_empty() {
        let mut by_kind = std::collections::BTreeMap::<String, u32>::new();
        for k in &all_blind_spot_kinds {
            *by_kind.entry(k.clone()).or_insert(0) += 1;
        }
        let files_field: serde_json::Value = if target_file_paths.len() == 1 {
            json!(target_file_paths[0])
        } else {
            json!(target_file_paths)
        };
        result_obj["blind_spot_warning"] = json!({
            "file": files_field,
            "total": all_blind_spot_kinds.len(),
            "by_kind": by_kind,
            "note": "traversal may be incomplete — see `ecp doctor` blind spots catalog",
        });
    }

    if args.test_coverage {
        let analyses = coverage_analyses(
            graph,
            view,
            &per_match_bfs,
            &args.direction,
            args.depth,
            min_conf,
            effective_include_tests,
            &rel_filter,
        );
        result_obj["coverage"] = build_coverage_json(analyses);
    }

    // FU-2026-05-29-011: with ≥2 same-named defs in the graph, the resolver
    // suppressed every bare call to this name at index time
    // (`DecisionTier::AmbiguousGlobal`), so the upstream caller set is a
    // lower bound — the payload must say so instead of reading as complete.
    let ambiguity_caveat = (same_name_defs >= 2
        && matches!(args.direction, Direction::Up | Direction::Both))
    .then(|| {
        format!(
            "caller set may be incomplete: {same_name_defs} same-named definitions of \
             '{bare_name}' exist, so bare calls (no import/qualifier context) were \
             ambiguity-suppressed at index time. Cross-check call sites with grep \
             before trusting the blast radius."
        )
    });

    Ok((
        result_obj,
        ImpactHints {
            empty_hint_name: emit_empty_hint.then(|| format_fqn(owner_filter, bare_name)),
            empty_hint_is_field,
            hidden_edges: hidden_edges_total,
            hidden_heuristic_edges: hidden_heuristic_total,
            ambiguity_caveat,
        },
    ))
}

pub(super) fn collect_blind_spots(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    target_file_path: &str,
) -> Vec<String> {
    graph
        .blind_spots
        .iter()
        .filter(|bs| bs.file_path.resolve(&graph.string_pool) == target_file_path)
        .map(|bs| bs.kind.resolve(&graph.string_pool).to_string())
        .collect()
}
