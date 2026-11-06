use super::bfs::{merged_node_is_test, merged_node_meta, run_bfs, WalkOpts};
use super::coverage::{build_coverage_json, coverage_analyses};
use super::payload::SymbolImpactPayload;
use super::unexplained::{self, is_vendored, UnexplainedRefs, UNEXPLAINED_SCAN_MAX_CALLERS};
use super::{
    parse_csv_lower, resolve_min_conf, ImpactArgs, ImpactHints, DEFAULT_CONFIDENCE_THRESHOLD,
};
use crate::commands::impact::{
    attach_heuristic_fields, attach_hidden_edges, attach_hidden_test_callers, direction_str,
    Direction,
};
use crate::commands::symbol_id::same_name_def_count;
use crate::engine::Engine;
use crate::read_context::{ambiguity_candidates, resolve_target, shell_word, target_request};
use ecp_core::contracts::identity::{Ambiguity, TargetRequest, TargetResolution};
use ecp_core::graph::RelType;
use ecp_core::session::MergedGraph;
use ecp_core::EcpError;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// Candidates an ambiguous target lists, most-called first.
const MAX_CANDIDATES: usize = 5;

/// Callers each ambiguous candidate names as evidence.
const TOP_CALLERS: usize = 3;

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
        // A library caller fans out over many symbols, as `--batch` does, so
        // it keeps the opt-in text scan (`scan_runs`).
        batch: true,
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
    let merged = MergedGraph::new(graph, view);
    let request = target_request(name, args.kind.as_deref(), args.file.as_deref())?;
    let bare_name = request.name();

    // A narrowing flag that leaves 2+ declarations (two `load` overloads,
    // `--kind method`) must not merge unrelated blast radii.
    let (start_idx, target_kind) = match resolve_target(&merged, &request) {
        TargetResolution::Unique(c) => (c.node as usize, c.kind),
        TargetResolution::Ambiguous(a) => return Err(a.to_error(&request)),
        TargetResolution::NotFound { named } => {
            return Err(not_found(&request, named, engine.unverified_note()))
        }
    };
    let same_name_defs = same_name_def_count(graph, view, bare_name, target_kind);

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

    let upstream = matches!(args.direction, Direction::Up | Direction::Both);
    let scan = (upstream
        && scan_runs(args, same_name_defs, || {
            direct_callers(merged, start_idx as u32).len()
        }))
    .then(|| {
        // `--repo` may be a registry name, not a path: the overlay's worktree
        // top is the work tree this graph answers for.
        let repo = engine
            .worktree_root()
            .or_else(|| args.repo.as_deref().map(Path::new).filter(|p| p.is_dir()))
            .unwrap_or(Path::new("."));
        unexplained::scan(merged, bare_name, repo)
    });

    // Blind spots scoped to this answer's evidence: an upstream walk counts
    // the files of the unexplained sites, where a construct the parser
    // cannot read may hold a caller; a downstream walk counts the target's
    // own file, where it may hold a callee.
    let target_file = merged_node_meta(graph, view, start_idx).file_path;
    let mut spot_files: BTreeSet<&str> = scan
        .as_ref()
        .map(UnexplainedRefs::files)
        .unwrap_or_default();
    if matches!(args.direction, Direction::Down | Direction::Both) {
        spot_files.insert(&target_file);
    }
    let blind_spots = collect_blind_spots(graph, &spot_files);
    // `coverage_analyses` reads the walk behind `--test-coverage` only, both
    // buckets; the default path moves the results into the payload once.
    let coverage_walk = args.test_coverage.then(|| {
        let rows = all_results.iter().chain(&all_heuristic_results).cloned();
        vec![(start_idx, rows.collect::<Vec<_>>())]
    });

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

    if let Some(warning) = blind_spot_warning(&blind_spots, &args.direction) {
        result_obj["blind_spot_warning"] = warning;
    }

    if let Some(walk) = &coverage_walk {
        let opts = WalkOpts {
            direction: args.direction.clone(),
            depth: args.depth,
            min_conf,
            include_tests: effective_include_tests,
            rel_filter: &rel_filter,
            include_heuristic: !args.no_heuristic,
        };
        let analyses = coverage_analyses(graph, view, walk, &opts);
        result_obj["coverage"] = build_coverage_json(analyses);
    }

    // FU-2026-05-29-011: with ≥2 same-named definitions of a competing kind
    // in the index (counting base defs an uncommitted edit renamed away), the
    // resolver may have suppressed bare calls to this name at index time
    // (`DecisionTier::AmbiguousGlobal`; language and vendor barriers exempt
    // some), so the upstream caller set is a lower bound — the payload must
    // say so instead of reading as complete, and cite what the scan found.
    let ambiguity_caveat = (same_name_defs >= 2 && upstream).then(|| {
        let action = match &scan {
            Some(refs) => match (refs.error(), refs.total()) {
                (Some(reason), _) => format!(
                    "The text scan for references the graph does not explain failed \
                     ({reason})."
                ),
                (None, 0) => {
                    "The text scan found no reference the graph does not explain.".to_string()
                }
                (None, n) => format!(
                    "`unexplained_refs` lists the {n} text reference(s) the graph does not \
                     explain."
                ),
            },
            None => format!(
                "Run `{}` to list the text references the graph does not explain.",
                ambiguous_callers_command(name, args.file.as_deref(), args.kind.as_deref())
            ),
        };
        format!(
            "caller set may be incomplete: {same_name_defs} same-named definitions of \
             '{bare_name}' exist in the index or working tree, so bare calls (no \
             import/qualifier context) may be unattributed. {action}"
        )
    });
    if let Some(refs) = &scan {
        result_obj["unexplained_refs"] = refs.to_json();
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
            unexplained_refs: scan.as_ref().map(|refs| match refs.error() {
                Some(reason) => Err(reason.to_string()),
                None => Ok(refs.total()),
            }),
        },
    ))
}

/// Whether the `unexplained_refs` text scan runs on an upstream walk. With
/// `batch` set (`--batch`, and the library callers that fan out over many
/// symbols) the scan stays opt-in, as before: one tracked-file scan per target
/// would undo the graph load they amortise. `direct_callers` runs last: a
/// colliding name or the flag decides first.
fn scan_runs(
    args: &ImpactArgs,
    same_name_defs: usize,
    direct_callers: impl FnOnce() -> usize,
) -> bool {
    match args.batch {
        true => args.ambiguous_callers && same_name_defs >= 2,
        false => {
            args.ambiguous_callers
                || same_name_defs >= 2
                || direct_callers() <= UNEXPLAINED_SCAN_MAX_CALLERS
        }
    }
}

/// The non-test nodes an upstream walk reaches at depth 1 through a resolved
/// edge: the sources of the in-edges of `idx`, of the other parts of its C#
/// `partial` type and of its constructors, minus containment, `PartOf` and
/// heuristic edges. Sorted by merged index, each once.
pub(super) fn direct_callers(merged: MergedGraph<'_>, idx: u32) -> Vec<u32> {
    let mut seeds = merged.partial_parts(idx);
    if seeds.is_empty() {
        seeds.push(idx);
    }
    let constructors: Vec<u32> = seeds
        .iter()
        .flat_map(|&s| merged.constructors_of(s))
        .collect();
    seeds.extend(constructors);
    let mut callers: Vec<u32> = seeds
        .iter()
        .flat_map(|&s| merged.in_edges(s))
        .filter(|e| {
            let rel = e.rel_type();
            !rel.is_scope_containment() && rel != RelType::PartOf && !rel.is_heuristic()
        })
        .map(|e| e.source)
        .filter(|s| !seeds.contains(s))
        .filter(|&s| {
            merged
                .node(s)
                .is_some_and(|n| n.file_path(&merged).is_some())
                && !merged_node_is_test(&merged, s)
        })
        .collect();
    callers.sort_unstable();
    callers.dedup();
    callers
}

/// The first [`TOP_CALLERS`] of `callers` by file and line, as `name file:line`.
fn top_callers(merged: MergedGraph<'_>, callers: &[u32]) -> Vec<String> {
    let mut rows: Vec<(&str, u32, &str)> = callers
        .iter()
        .filter_map(|&c| {
            let node = merged.node(c)?;
            Some((
                node.file_path(&merged)?,
                node.start_line(),
                node.name(&merged),
            ))
        })
        .collect();
    rows.sort_unstable();
    rows.into_iter()
        .take(TOP_CALLERS)
        .map(|(file, line, name)| format!("{name} {file}:{line}"))
        .collect()
}

/// The payload fields of an ambiguous target: at most [`MAX_CANDIDATES`]
/// candidates, most-called first (ties keep the resolver's order), each with
/// the arguments that select it and its callers as evidence. A candidate in
/// a vendored directory is dropped unless every candidate is in one. `more`
/// counts the candidates not shown, dropped ones included, and names the
/// command that lists them all. The agent picks; this never does.
pub(super) fn ambiguous_candidates(
    merged: MergedGraph<'_>,
    ambiguity: &Ambiguity,
    name: &str,
) -> Map<String, Value> {
    let selectors = ambiguity_candidates(ambiguity);
    let selectors = selectors.as_array().map(Vec::as_slice).unwrap_or_default();
    let all_vendored = ambiguity.candidates().iter().all(|c| is_vendored(&c.file));
    let mut rows: Vec<(usize, Value)> = ambiguity
        .candidates()
        .iter()
        .zip(selectors)
        .filter(|(c, _)| all_vendored || !is_vendored(&c.file))
        .map(|(c, selector)| {
            let callers = direct_callers(merged, c.node);
            let row = json!({
                "target": selector["target"],
                "file": c.file,
                "line": c.line,
                "kind": selector["kind"],
                "callers": callers.len(),
                "top_callers": top_callers(merged, &callers),
            });
            (callers.len(), row)
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    rows.truncate(MAX_CANDIDATES);
    let more = ambiguity.total() - rows.len();
    let mut fields = Map::new();
    fields.insert(
        "candidates".to_string(),
        rows.into_iter().map(|(_, row)| row).collect(),
    );
    if more > 0 {
        fields.insert("more".to_string(), json!(more));
        fields.insert(
            "more_command".to_string(),
            json!(format!("ecp find {} --all", shell_word(name))),
        );
    }
    fields
}

/// The exact command that lists the unexplained references, carrying the
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

/// The miss an agent reads: whether the name is declared at all, the next
/// command when it is not, and `unverified` when the graph may lack HEAD.
fn not_found(request: &TargetRequest, named: u32, unverified: Option<String>) -> EcpError {
    let target = request.render();
    let miss = match named {
        0 => format!(
            "No symbol named '{target}' found in graph. Try `ecp find <name> --mode fuzzy` to find candidates, or check --file / --kind filters"
        ),
        n => format!(
            "No symbol named '{target}' found in graph: {n} declaration(s) named '{}' exist, but none passes the owner, @line, --file or --kind filter. Run `ecp impact --target {}` to list them",
            request.name(),
            shell_word(request.name()),
        ),
    };
    EcpError::InvalidArgument(match unverified {
        Some(note) => format!("{miss}. {note}"),
        None => miss,
    })
}

/// The `(file, kind)` of every blind spot in `files`.
pub(super) fn collect_blind_spots<'g>(
    graph: &'g ecp_core::graph::ArchivedZeroCopyGraph,
    files: &BTreeSet<&str>,
) -> Vec<(&'g str, &'g str)> {
    if files.is_empty() {
        return Vec::new();
    }
    graph
        .blind_spots
        .iter()
        .filter_map(|bs| {
            let file = bs.file_path.resolve(&graph.string_pool);
            files
                .contains(file)
                .then(|| (file, bs.kind.resolve(&graph.string_pool)))
        })
        .collect()
}

/// The `blind_spot_warning` object, `None` when no blind spot is in scope.
/// `files` lists the files that hold the counted blind spots, always as an
/// array.
fn blind_spot_warning(spots: &[(&str, &str)], direction: &Direction) -> Option<Value> {
    if spots.is_empty() {
        return None;
    }
    let files: BTreeSet<&str> = spots.iter().map(|&(file, _)| file).collect();
    let mut by_kind = BTreeMap::<&str, u32>::new();
    for &(_, kind) in spots {
        *by_kind.entry(kind).or_insert(0) += 1;
    }
    let scope = match direction {
        Direction::Up => {
            "blind spots in the files of the `unexplained_refs` sites, where a construct \
             the parser cannot read may hold a caller"
        }
        Direction::Down => {
            "blind spots in the target's own file, where a construct the parser cannot \
             read may hold a callee"
        }
        Direction::Both => {
            "blind spots in the files of the `unexplained_refs` sites (a possible caller) \
             and in the target's own file (a possible callee)"
        }
    };
    Some(json!({
        "files": files,
        "total": spots.len(),
        "by_kind": by_kind,
        "note": format!("{scope} — see `ecp doctor` blind spots catalog"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blind_spot_warning_files_array_and_note_name_the_scope() {
        let spots = [
            ("app/Svc.php", "php-variable-call"),
            ("app/Svc.php", "php-variable-call"),
            ("app/Use.php", "php-dynamic-new"),
        ];
        let up = blind_spot_warning(&spots, &Direction::Up).unwrap();
        assert_eq!(up["files"], json!(["app/Svc.php", "app/Use.php"]));
        assert_eq!(up["total"], 3);
        assert_eq!(up["by_kind"]["php-variable-call"], 2);
        let note = up["note"].as_str().unwrap();
        assert!(note.contains("`unexplained_refs` sites"), "{note}");
        let down = blind_spot_warning(&spots[..1], &Direction::Down).unwrap();
        assert_eq!(
            down["files"],
            json!(["app/Svc.php"]),
            "an array, never a scalar"
        );
        assert!(down["note"].as_str().unwrap().contains("target's own file"));
    }

    #[test]
    fn test_blind_spot_warning_no_spot_none() {
        assert!(blind_spot_warning(&[], &Direction::Both).is_none());
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
