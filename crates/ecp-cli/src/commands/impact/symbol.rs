use super::bfs::{merged_node_meta, run_bfs};
use super::coverage::{build_coverage_json, coverage_analyses};
use super::payload::SymbolImpactPayload;
use super::{
    parse_csv_lower, resolve_min_conf, ImpactArgs, ImpactHints, DEFAULT_CONFIDENCE_THRESHOLD,
};
use crate::commands::impact::{
    attach_heuristic_fields, attach_hidden_edges, attach_hidden_test_callers, direction_str,
    Direction,
};
use crate::commands::symbol_id::same_name_def_count;
use crate::engine::Engine;
use crate::read_context::{resolve_target, shell_word, target_request};
use ecp_core::contracts::identity::{TargetRequest, TargetResolution};
use ecp_core::session::MergedGraph;
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::path::Path;

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
        include_tests: false,
        exclude_tests: !include_tests,
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
        ambiguous_callers: false,
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
    let request = target_request(name, args.kind.as_deref(), args.file.as_deref())?;
    let bare_name = request.name();

    // A narrowing flag that leaves 2+ declarations (two `load` overloads,
    // `--kind method`) must not merge unrelated blast radii.
    let start_idx = match resolve_target(&MergedGraph::new(graph, view), &request) {
        TargetResolution::Unique(c) => c.node as usize,
        TargetResolution::Ambiguous(a) => return Err(a.to_error(&request)),
        TargetResolution::NotFound { named } => return Err(not_found(&request, named)),
    };
    let same_name_defs = same_name_def_count(graph, view, bare_name);

    let min_conf = resolve_min_conf(args);
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    let effective_include_tests = args.walks_tests();

    // `max_results` is the budget of the whole call; 0 walks nothing.
    let (
        all_results,
        all_heuristic_results,
        hidden_edges_total,
        hidden_heuristic_total,
        hidden_test_total,
    ) = match args.max_results {
        Some(0) => (Vec::new(), Vec::new(), 0, 0, 0),
        remaining => run_bfs(
            graph,
            view,
            start_idx,
            &args.direction,
            args.depth,
            min_conf,
            effective_include_tests,
            &rel_filter,
            !args.no_heuristic,
            remaining,
        ),
    };

    // Empty callers hint for upstream direction. A heuristic caller is still a
    // caller: the hint would otherwise call the target uncalled or test-only.
    let reached_beyond_start = !all_heuristic_results.is_empty()
        || all_results
            .iter()
            .any(|e| e["depth"].as_u64().unwrap_or(0) > 0);
    let emit_empty_hint = !reached_beyond_start && args.direction == Direction::Up;
    // A field target with no readers: the hint must flag that some languages
    // don't model field reads yet, so empty != provably unread.
    let empty_hint_is_field = emit_empty_hint
        && all_results
            .iter()
            .any(|e| e["depth"].as_u64() == Some(0) && e["kind"].as_str() == Some("property"));

    let target_file = merged_node_meta(graph, view, start_idx).file_path;
    let all_blind_spot_kinds = collect_blind_spots(graph, &target_file);
    // `coverage_analyses` reads the walk behind `--test-coverage` only; the
    // default path moves the results into the payload once.
    let coverage_walk = args
        .test_coverage
        .then(|| vec![(start_idx, all_results.clone())]);

    // Use the original user-supplied name (which may be FQN) as the target
    // label in output — more precise than bare_name when owner was specified.
    let payload = SymbolImpactPayload {
        status: "success".to_string(),
        target: name.to_string(),
        direction: direction_str(&args.direction).to_string(),
        impact: all_results,
    };
    let mut result_obj =
        serde_json::to_value(&payload).map_err(|e| EcpError::Serialization(e.to_string()))?;
    attach_hidden_edges(&mut result_obj, hidden_edges_total);
    attach_hidden_test_callers(&mut result_obj, hidden_test_total);
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
        result_obj["blind_spot_warning"] =
            blind_spot_warning(json!(target_file), all_blind_spot_kinds.len(), &by_kind);
    }

    if let Some(walk) = &coverage_walk {
        let analyses = coverage_analyses(
            graph,
            view,
            walk,
            &args.direction,
            args.depth,
            min_conf,
            effective_include_tests,
            &rel_filter,
        );
        result_obj["coverage"] = build_coverage_json(analyses);
    }

    // FU-2026-05-29-011: with ≥2 same-named defs in the index (counting base
    // defs an uncommitted edit renamed away), the resolver
    // may have suppressed bare calls to this name at index time
    // (`DecisionTier::AmbiguousGlobal`; language and vendor barriers exempt
    // some), so the upstream caller set is a lower bound — the payload must
    // say so instead of reading as complete.
    let ambiguity_caveat = (same_name_defs >= 2
        && matches!(args.direction, Direction::Up | Direction::Both))
    .then(|| {
        let action = match args.ambiguous_callers {
            true => "The `ambiguous_callers` field lists the call sites the graph could not \
                     attribute."
                .to_string(),
            false => format!(
                "Run `{}` to list the call sites the graph could not attribute.",
                ambiguous_callers_command(name, args.file.as_deref(), args.kind.as_deref())
            ),
        };
        format!(
            "caller set may be incomplete: {same_name_defs} same-named definitions of \
             '{bare_name}' exist in the index or working tree, so bare calls (no \
             import/qualifier context) may be unattributed. {action}"
        )
    });
    if args.ambiguous_callers && ambiguity_caveat.is_some() {
        let repo = Path::new(args.repo.as_deref().unwrap_or("."));
        result_obj["ambiguous_callers"] =
            super::ambiguous::ambiguous_callers(MergedGraph::new(graph, view), bare_name, repo);
    }

    Ok((
        result_obj,
        ImpactHints {
            empty_hint_name: emit_empty_hint.then(|| name.to_string()),
            empty_hint_is_field,
            hidden_edges: hidden_edges_total,
            hidden_heuristic_edges: hidden_heuristic_total,
            hidden_test_callers: hidden_test_total,
            payload_caveat: ambiguity_caveat,
        },
    ))
}

/// The exact command that lists the unattributed call sites, carrying the
/// `--file` / `--kind` that narrowed this run: a colliding name without them
/// is rejected as ambiguous, so a bare `--target` would not run.
fn ambiguous_callers_command(name: &str, file: Option<&str>, kind: Option<&str>) -> String {
    let mut cmd = format!("ecp impact --target {}", shell_word(name));
    if let Some(file) = file {
        let _ = write!(cmd, " --file {}", shell_word(file));
    }
    if let Some(kind) = kind {
        let _ = write!(cmd, " --kind {}", shell_word(kind));
    }
    cmd.push_str(" --ambiguous-callers");
    cmd
}

/// The miss an agent reads: whether the name is declared at all, and the
/// next command when it is not.
fn not_found(request: &TargetRequest, named: u32) -> EcpError {
    let target = request.render();
    EcpError::InvalidArgument(match named {
        0 => format!(
            "No symbol named '{target}' found in graph. Try `ecp find <name> --mode fuzzy` to find candidates, or check --file / --kind filters"
        ),
        n => format!(
            "No symbol named '{target}' found in graph: {n} declaration(s) named '{}' exist, but none passes the owner, @line, --file or --kind filter. Run `ecp impact --target {}` to list them",
            request.name(),
            shell_word(request.name()),
        ),
    })
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

/// The counts cover the target's own file only; a blind spot that hides a
/// caller sits in the caller's file, so the note says those files are unread.
fn blind_spot_warning(
    files: Value,
    total: usize,
    by_kind: &std::collections::BTreeMap<String, u32>,
) -> Value {
    json!({
        "file": files,
        "total": total,
        "by_kind": by_kind,
        "note": "counts cover only the target's own file(s) listed in `file`; blind spots in callers' files are not included, so traversal may be incomplete — see `ecp doctor` blind spots catalog",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blind_spot_warning_note_names_scope_and_excludes_callers() {
        let by_kind = std::collections::BTreeMap::from([("php-variable-call".to_string(), 6)]);
        let w = blind_spot_warning(json!("app/Svc.php"), 6, &by_kind);
        let note = w["note"].as_str().unwrap();
        assert_eq!(w["file"], "app/Svc.php");
        assert!(note.contains("only the target's own file"), "{note}");
        assert!(note.contains("callers' files are not included"), "{note}");
    }

    #[test]
    fn test_ambiguous_callers_command_path_with_space_is_single_quoted() {
        assert_eq!(
            ambiguous_callers_command("get", Some("my dir/a.py"), Some("function")),
            "ecp impact --target get --file 'my dir/a.py' --kind function --ambiguous-callers"
        );
    }

    #[test]
    fn test_shell_word_metacharacters_and_quotes_escaped() {
        assert_eq!(shell_word("src/a-b_c.py"), "src/a-b_c.py");
        assert_eq!(shell_word("Owner.Method"), "Owner.Method");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word("a;b"), "'a;b'");
        assert_eq!(shell_word("$(x)"), "'$(x)'");
        assert_eq!(shell_word(""), "''");
    }
}
