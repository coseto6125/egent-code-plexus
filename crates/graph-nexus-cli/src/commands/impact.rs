use crate::commands::format::{kind_to_str, rel_to_str};
use crate::engine::Engine;
use crate::git::{DiffScope, GitDiffProvider, ShellGitProvider};
use crate::output::{emit, OutputFormat};
use crate::reanalyze::make_pipeline;
use clap::{Args, ValueEnum};
use graph_nexus_core::algorithms::process_trace::is_test_path;
use graph_nexus_core::config;
use graph_nexus_core::graph::NodeKind;
use graph_nexus_core::{GnxError, HIGH_TRUST_CONFIDENCE};
use rayon::prelude::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum Direction {
    #[value(alias = "upstream")]
    Up,
    #[value(alias = "downstream")]
    Down,
    Both,
}

/// Symbol-level blast radius. From `<name>` traverses call-graph for upstream
/// callers / downstream callees with risk_level. From `--baseline <ref>`
/// detects symbols changed vs the baseline and runs the same traversal per
/// change. For edge-level resolver delta (tier degradation, silent break),
/// use `gnx diff --section bindings` instead.
#[derive(Args, Debug)]
pub struct ImpactArgs {
    /// Target symbol name (mutually exclusive with --baseline). Equivalent to
    /// the `--target` named form below.
    pub name: Option<String>,

    /// Named alias for the positional NAME argument — kept for parity with
    /// old MCP / wrapper habits.
    #[arg(long = "target", value_name = "TARGET", conflicts_with_all = ["name", "baseline"])]
    pub target: Option<String>,

    /// Git ref — compute blast radius across all symbols changed between
    /// this baseline and HEAD. Mutually exclusive with positional <name>.
    #[arg(long, conflicts_with = "name")]
    pub baseline: Option<String>,

    /// Disambiguate when name has multiple matches: substring on file path.
    #[arg(long = "file_path", alias = "file-path")]
    pub file: Option<String>,

    /// Disambiguate by kind (function | method | class | route | ...).
    #[arg(long)]
    pub kind: Option<String>,

    /// Direction of traversal.
    #[arg(long, value_enum, default_value_t = Direction::Up)]
    pub direction: Direction,

    /// Maximum BFS depth.
    #[arg(long, default_value_t = 5)]
    pub depth: usize,

    /// Default OFF — recall-first: traverse every edge regardless of
    /// confidence (cross-crate refs at 0.7 are still real callers, just
    /// less certain). Pass `--high-trust-only=true` to restrict to
    /// confidence ≥ 0.8 edges for a noise-light view; when filtering kicks
    /// in, the output reports `hidden_edges` so missed coverage stays
    /// visible.
    #[arg(long, alias = "high_trust_only", default_value_t = false, action = clap::ArgAction::Set)]
    pub high_trust_only: bool,

    /// Override the high-trust threshold with a custom value (0.0–1.0).
    /// If set, takes precedence over --high-trust-only.
    #[arg(long, alias = "min_confidence")]
    pub min_confidence: Option<f32>,

    /// Include test files in traversal.
    #[arg(long, aliases = ["include_tests", "includeTests"], default_value_t = false)]
    pub include_tests: bool,

    /// Comma-separated relation types to follow (calls, extends, ...).
    #[arg(long = "relation_types", alias = "relation-types")]
    pub relation_types: Option<String>,

    /// Repository selector.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format (mostly internal — agent doesn't set this).
    #[arg(long)]
    pub format: Option<String>,
}

/// Split a comma-separated flag value into a normalized lowercase Vec.
/// Empty / whitespace-only parts are dropped so `--kind ,function,` works.
fn parse_csv_lower(s: Option<&str>) -> Option<Vec<String>> {
    s.map(|raw| {
        raw.split(',')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .collect()
    })
}

/// Stderr hints produced during impact computation. Collected by helpers and
/// emitted by `run` so that library callers via `build_payload` stay stderr-clean.
#[derive(Default)]
struct ImpactStderrHints {
    empty_hint_name: Option<String>,
    /// If > 0, emit the hidden-edges footer.
    hidden_edges: u64,
}

pub fn run(args: ImpactArgs, engine: &Engine) -> Result<(), GnxError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let (payload, hints) = build_payload_with_hints(&args, engine)?;
    if let Some(name) = &hints.empty_hint_name {
        eprintln!(
            "→ \"{name}\" exists but has 0 incoming references. Possible: entry point, dead code, or recent rename. Try --direction both / --include-tests"
        );
    }
    emit_hidden_edges_footer(hints.hidden_edges);
    emit(&payload, format)
}

/// Library API: returns the JSON payload only, dropping stderr hints.
///
/// `run` (binary path) calls `build_payload_with_hints` directly so it can
/// print the hints to stderr, which means this thin wrapper has no in-crate
/// caller and `cargo` flags it as dead. Kept `pub` to mirror the 5-command
/// `build_payload` surface introduced in PR #88 for future library consumers.
#[allow(dead_code)]
pub fn build_payload(args: &ImpactArgs, engine: &Engine) -> Result<Value, GnxError> {
    build_payload_with_hints(args, engine).map(|(v, _)| v)
}

// ── Per-symbol library API (used by `gnx group impact`) ─────────────────────

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

    /// The full JSON payload — same shape as `gnx impact --format json`.
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
/// Returns `Ok(LocalImpact)` even when the symbol is not found in the graph
/// (the payload will carry an `"error"` field in that case), matching the
/// same graceful-degradation behaviour as `gnx impact --target X`.
pub fn run_for_symbol(
    engine: &Engine,
    member_repo: &str,
    target: &str,
    direction: &str,
    max_depth: Option<u32>,
    timeout_ms: Option<u64>,
    include_tests: bool,
) -> Result<LocalImpact, GnxError> {
    let dir = match direction.to_ascii_lowercase().as_str() {
        "downstream" | "down" => Direction::Down,
        "both" => Direction::Both,
        _ => Direction::Up,
    };
    let args = ImpactArgs {
        name: Some(target.to_string()),
        target: None,
        baseline: None,
        file: None,
        kind: None,
        direction: dir,
        depth: max_depth.unwrap_or(5) as usize,
        high_trust_only: false,
        min_confidence: None,
        include_tests,
        relation_types: None,
        repo: Some(member_repo.to_string()),
        format: None,
    };
    let _ = timeout_ms; // timeout enforcement is caller-side; passed for API parity
    let (payload, _hints) = build_payload_with_hints(&args, engine)?;
    Ok(LocalImpact { payload })
}

fn build_payload_with_hints(
    args: &ImpactArgs,
    engine: &Engine,
) -> Result<(Value, ImpactStderrHints), GnxError> {
    let has_name = args.name.is_some() || args.target.is_some();
    match (has_name, args.baseline.as_ref()) {
        (true, None) => impact_by_name(args, engine),
        (false, Some(_)) => {
            impact_with_baseline(args, engine).map(|v| (v, ImpactStderrHints::default()))
        }
        (false, None) => Err(GnxError::InvalidArgument(
            "impact requires a symbol (positional <name> or --target <name>) or --baseline <ref>"
                .into(),
        )),
        (true, Some(_)) => unreachable!("clap conflicts_with prevents this"),
    }
}

fn impact_by_name(
    args: &ImpactArgs,
    engine: &Engine,
) -> Result<(Value, ImpactStderrHints), GnxError> {
    let name = args
        .name
        .as_deref()
        .or(args.target.as_deref())
        .expect("build_payload_with_hints gates on name||target");
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    // Resolve name → matching node indices, with optional --file / --kind disambiguation.
    let file_needle = args.file.as_deref();
    let kind_needle = args.kind.as_deref().map(|s| s.to_ascii_lowercase());

    let matches: Vec<usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            if node.name.resolve(&graph.string_pool) != name {
                return false;
            }
            if let Some(ref kn) = kind_needle {
                let node_kind = kind_to_str(&node.kind).to_ascii_lowercase();
                if &node_kind != kn {
                    return false;
                }
            }
            if let Some(needle) = file_needle {
                let file_path = graph.files[node.file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.string_pool);
                if !file_path.contains(needle) {
                    return false;
                }
            }
            true
        })
        .map(|(i, _)| i)
        .collect();

    if matches.is_empty() {
        return Ok((
            json!({
                "error": format!("No symbol named '{name}' found in graph"),
                "hint": "Try `gnx find <name> --mode fuzzy` to find candidates, or check --file / --kind filters"
            }),
            ImpactStderrHints::default(),
        ));
    }

    // Multiple matches without disambiguation → report candidates then fail.
    if matches.len() > 1 && file_needle.is_none() && kind_needle.is_none() {
        let candidates: Vec<Value> = matches
            .iter()
            .map(|&i| {
                let node = &graph.nodes[i];
                let file_path = graph.files[node.file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.string_pool);
                json!({
                    "kind": kind_to_str(&node.kind),
                    "filePath": file_path,
                    "line": node.span.0.to_native(),
                })
            })
            .collect();
        return Ok((
            json!({
                "error": format!("'{name}' is ambiguous ({} candidates) — add --file or --kind to disambiguate", matches.len()),
                "candidates": candidates,
            }),
            ImpactStderrHints::default(),
        ));
    }

    let min_conf = resolve_min_conf(&args);
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());

    let mut all_results: Vec<Value> = Vec::new();
    let mut hidden_edges_total: u64 = 0;
    for start_idx in &matches {
        let (bfs_result, hidden) = run_bfs(
            graph,
            *start_idx,
            &args.direction,
            args.depth,
            min_conf,
            args.include_tests,
            &rel_filter,
        );
        all_results.extend(bfs_result);
        hidden_edges_total += hidden;
    }

    // Empty callers hint for upstream direction.
    let impact_without_start: Vec<&Value> = all_results
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
        .collect();
    let emit_empty_hint = impact_without_start.is_empty() && args.direction == Direction::Up;

    // Collect unique file paths across ALL matches so the blind-spot warning
    // is accurate when --file / --kind still leaves >1 match.
    let mut seen_files = HashSet::new();
    let target_file_paths: Vec<String> = matches
        .iter()
        .map(|&idx| {
            let file_idx = graph.nodes[idx].file_idx.to_native() as usize;
            graph.files[file_idx]
                .path
                .resolve(&graph.string_pool)
                .to_string()
        })
        .filter(|p| seen_files.insert(p.clone()))
        .collect();

    let mut all_blind_spot_kinds: Vec<String> = Vec::new();
    for fp in &target_file_paths {
        all_blind_spot_kinds.extend(collect_blind_spots(graph, fp));
    }

    let mut result_obj = json!({
        "status": "success",
        "target": name,
        "direction": direction_str(&args.direction),
        "impact": all_results,
    });
    attach_hidden_edges(&mut result_obj, hidden_edges_total);

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
            "note": "traversal may be incomplete — see `gnx doctor` blind spots catalog",
        });
    }

    Ok((
        result_obj,
        ImpactStderrHints {
            empty_hint_name: emit_empty_hint.then(|| name.to_string()),
            hidden_edges: hidden_edges_total,
        },
    ))
}

fn impact_with_baseline(args: &ImpactArgs, engine: &Engine) -> Result<Value, GnxError> {
    let baseline_ref = args.baseline.as_deref().unwrap();
    let repo_path = PathBuf::from(args.repo.as_deref().unwrap_or("."));

    let scope = DiffScope::Compare(baseline_ref.to_string());
    let provider = ShellGitProvider;
    let file_diffs = provider.diff(&repo_path, &scope)?;

    if file_diffs.is_empty() {
        return Ok(json!({
            "status": "success",
            "baseline": baseline_ref,
            "message": "0 changes detected — no symbols to assess",
            "changed_symbols": [],
            "impact_by_symbol": [],
        }));
    }

    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    // Identify changed file paths from the diff.
    let changed_paths: Vec<String> = file_diffs
        .iter()
        .filter(|fd| args.include_tests || !is_test_path(&fd.file_path))
        .map(|fd| fd.file_path.clone())
        .collect();

    // Re-parse new and old side per changed file. Each iteration is
    // independent (writes only into its own local vectors), and tree-sitter
    // parse + `git show` subprocess dominate the work — fan out via rayon
    // and merge at the end. `pipeline.parse_file_raw` is the same call path
    // that `pipeline.analyze`'s `into_par_iter` already uses, so providers
    // are Send + Sync by construction.
    let pipeline = make_pipeline();
    type NewEntry = ((&'static str, String, String), (u64, u32));
    type OldEntry = ((&'static str, String, String), u64);

    let per_file: Vec<(Vec<NewEntry>, Vec<OldEntry>)> = changed_paths
        .par_iter()
        .map(|rel_path| {
            let mut new_local: Vec<NewEntry> = Vec::new();
            let mut old_local: Vec<OldEntry> = Vec::new();

            let abs = repo_path.join(rel_path);
            if abs.exists() {
                if let Ok(src) = std::fs::read(&abs) {
                    let rel_pb = PathBuf::from(rel_path);
                    if let Ok(lg) = pipeline.parse_file_raw(&rel_pb, &src) {
                        let lines: Vec<&[u8]> = src.split(|&b| b == b'\n').collect();
                        for raw in &lg.nodes {
                            if matches!(raw.kind, NodeKind::File | NodeKind::Process) {
                                continue;
                            }
                            let h = hash_node_lines(&lines, raw.span.0, raw.span.2);
                            let kind_str = node_kind_to_str(&raw.kind);
                            new_local.push((
                                (kind_str, rel_path.clone(), raw.name.clone()),
                                (h, raw.span.0),
                            ));
                        }
                    }
                }
            }

            if let Some(old_src) = head_blob_at(&repo_path, rel_path, baseline_ref) {
                let rel_pb = PathBuf::from(rel_path);
                if let Ok(lg) = pipeline.parse_file_raw(&rel_pb, &old_src) {
                    let lines: Vec<&[u8]> = old_src.split(|&b| b == b'\n').collect();
                    for raw in &lg.nodes {
                        if matches!(raw.kind, NodeKind::File | NodeKind::Process) {
                            continue;
                        }
                        let h = hash_node_lines(&lines, raw.span.0, raw.span.2);
                        let kind_str = node_kind_to_str(&raw.kind);
                        old_local.push(((kind_str, rel_path.clone(), raw.name.clone()), h));
                    }
                }
            }

            (new_local, old_local)
        })
        .collect();

    let total_new = per_file.iter().map(|(n, _)| n.len()).sum();
    let total_old = per_file.iter().map(|(_, o)| o.len()).sum();
    let mut new_map: HashMap<(&'static str, String, String), (u64, u32)> =
        HashMap::with_capacity(total_new);
    let mut old_map: HashMap<(&'static str, String, String), u64> =
        HashMap::with_capacity(total_old);
    for (new_local, old_local) in per_file {
        new_map.extend(new_local);
        old_map.extend(old_local);
    }

    // Build lookup from old graph: (kind_str, file_path, name) → node_idx.
    let changed_files_set: HashSet<&str> = changed_paths.iter().map(|s| s.as_str()).collect();
    let mut old_graph_idx: HashMap<(&'static str, String, String), usize> = HashMap::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        let file_node = &graph.files[node.file_idx.to_native() as usize];
        let file_path = file_node.path.resolve(&graph.string_pool);
        if !changed_files_set.contains(file_path) {
            continue;
        }
        let kind_str = kind_to_str(&node.kind);
        let name = node.name.resolve(&graph.string_pool).to_string();
        old_graph_idx.insert((kind_str, file_path.to_string(), name), idx);
    }

    // Collect changed symbol keys + their graph indices.
    let mut changed_symbols: Vec<Value> = Vec::new();
    let mut changed_node_indices: Vec<usize> = Vec::new();

    for (key, (_, start_row)) in &new_map {
        if !old_map.contains_key(key) {
            changed_symbols.push(json!({
                "name": key.2,
                "kind": key.0,
                "filePath": key.1,
                "line": start_row,
                "change_type": "added",
            }));
        }
    }

    for (key, old_hash) in &old_map {
        match new_map.get(key) {
            Some((new_hash, start_row)) => {
                if old_hash != new_hash {
                    changed_symbols.push(json!({
                        "name": key.2,
                        "kind": key.0,
                        "filePath": key.1,
                        "line": start_row,
                        "change_type": "modified",
                    }));
                    if let Some(&idx) = old_graph_idx.get(key) {
                        if !changed_node_indices.contains(&idx) {
                            changed_node_indices.push(idx);
                        }
                    }
                }
            }
            None => {
                changed_symbols.push(json!({
                    "name": key.2,
                    "kind": key.0,
                    "filePath": key.1,
                    "line": 0u32,
                    "change_type": "removed",
                }));
                if let Some(&idx) = old_graph_idx.get(key) {
                    if !changed_node_indices.contains(&idx) {
                        changed_node_indices.push(idx);
                    }
                }
            }
        }
    }

    let min_conf = resolve_min_conf(&args);
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());

    // Run BFS from each changed symbol.
    let mut impact_by_symbol: Vec<Value> = Vec::new();
    let mut hidden_edges_total: u64 = 0;
    for &start_idx in &changed_node_indices {
        let node = &graph.nodes[start_idx];
        let sym_name = node.name.resolve(&graph.string_pool).to_string();
        let sym_file = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool)
            .to_string();
        let (bfs_result, hidden) = run_bfs(
            graph,
            start_idx,
            &args.direction,
            args.depth,
            min_conf,
            args.include_tests,
            &rel_filter,
        );
        impact_by_symbol.push(json!({
            "symbol": sym_name,
            "filePath": sym_file,
            "impact": bfs_result,
        }));
        hidden_edges_total += hidden;
    }

    let mut result = json!({
        "status": "success",
        "baseline": baseline_ref,
        "changed_symbols": changed_symbols,
        "impact_by_symbol": impact_by_symbol,
    });
    attach_hidden_edges(&mut result, hidden_edges_total);
    Ok(result)
}

/// Attach the hidden-edge count to the JSON result when filtering actually
/// dropped something. Skipping the field when N=0 keeps default invocations
/// noise-free and lets callers branch on `result.get("hidden_edges")`.
fn attach_hidden_edges(result: &mut Value, hidden_edges: u64) {
    if hidden_edges > 0 {
        result["hidden_edges"] = json!(hidden_edges);
    }
}

/// Stderr footer mirroring `attach_hidden_edges` — emitted only when the
/// trust filter dropped at least one edge, routed to stderr so it doesn't
/// corrupt machine-readable JSON/TOON on stdout.
fn emit_hidden_edges_footer(hidden_edges: u64) {
    if hidden_edges > 0 {
        eprintln!(
            "note: {hidden_edges} edges hidden by trust filter (drop --high-trust-only / --min-confidence to see all)"
        );
    }
}

/// Resolve the effective confidence threshold from `--min-confidence` /
/// `--high-trust-only` / repo config.
fn resolve_min_conf(args: &ImpactArgs) -> f32 {
    let repo_root = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let cfg_threshold = config::load(&repo_root)
        .map(|c| c.confidence.high_trust_threshold)
        .unwrap_or(HIGH_TRUST_CONFIDENCE);
    args.min_confidence.unwrap_or(if args.high_trust_only {
        cfg_threshold
    } else {
        0.0
    })
}

fn direction_str(dir: &Direction) -> &'static str {
    match dir {
        Direction::Up => "upstream",
        Direction::Down => "downstream",
        Direction::Both => "both",
    }
}

/// Core BFS over the graph from `start_idx`.
///
/// Returns `(visited_nodes, hidden_edges)`. The start node appears at
/// depth 0. `--include-tests` / `--relation-types` / `min_conf` are
/// applied here; `--kind` / `--file` emission-only filtering is NOT
/// applied here (callers can filter the returned Vec if needed).
///
/// `hidden_edges` counts edges dropped *because their confidence fell
/// below `min_conf`* — i.e. the surface area lost to the high-trust
/// filter. Edges skipped for other reasons (`--include-tests`,
/// `--relation-types`, already-visited target) are NOT counted, since
/// those are explicit user-driven filters rather than the silent default.
fn run_bfs(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    start_idx: usize,
    direction: &Direction,
    max_depth: usize,
    min_conf: f32,
    include_tests: bool,
    rel_filter: &Option<Vec<String>>,
) -> (Vec<Value>, u64) {
    type ViaEdge = Option<(String, f32)>;
    type Step = (usize, usize, ViaEdge);

    let mut visited = HashSet::new();
    let mut queue: VecDeque<Step> = VecDeque::new();
    let mut results = Vec::new();
    let mut test_path_cache = HashMap::new();
    let mut hidden_edges: u64 = 0;

    queue.push_back((start_idx, 0, None));
    visited.insert(start_idx);

    while let Some((curr_idx, curr_depth, via)) = queue.pop_front() {
        let curr_node = &graph.nodes[curr_idx];
        let file_idx = curr_node.file_idx.to_native() as usize;

        if !include_tests {
            let is_test = *test_path_cache.entry(file_idx).or_insert_with(|| {
                let file_path = graph.files[file_idx].path.resolve(&graph.string_pool);
                is_test_path(file_path)
            });
            if is_test {
                continue;
            }
        }

        let file_path = graph.files[file_idx]
            .path
            .resolve(&graph.string_pool)
            .to_string();
        let (via_reason, via_confidence) = via
            .as_ref()
            .map(|(r, c)| (r.as_str(), *c))
            .unwrap_or(("", 1.0));

        results.push(json!({
            "uid": curr_node.uid.resolve(&graph.string_pool),
            "name": curr_node.name.resolve(&graph.string_pool),
            "kind": kind_to_str(&curr_node.kind),
            "filePath": file_path,
            "line": curr_node.span.0.to_native(),
            "depth": curr_depth,
            "viaReason": via_reason,
            "viaConfidence": via_confidence,
        }));

        if curr_depth >= max_depth {
            continue;
        }

        match direction {
            Direction::Up | Direction::Both => {
                let in_start = graph.in_offsets[curr_idx].to_native() as usize;
                let in_end = graph.in_offsets[curr_idx + 1].to_native() as usize;
                for i in in_start..in_end {
                    let edge_idx = graph.in_edge_idx[i].to_native() as usize;
                    let edge = &graph.edges[edge_idx];
                    let edge_conf = edge.confidence.to_native();
                    if edge_conf < min_conf {
                        hidden_edges += 1;
                        continue;
                    }
                    if let Some(rels) = rel_filter.as_ref() {
                        let rel_str = rel_to_str(&edge.rel_type);
                        if !rels.iter().any(|r| r == rel_str) {
                            continue;
                        }
                    }
                    let next_idx = edge.source.to_native() as usize;
                    if !visited.contains(&next_idx) {
                        visited.insert(next_idx);
                        let edge_reason = edge.reason.resolve(&graph.string_pool).to_string();
                        queue.push_back((next_idx, curr_depth + 1, Some((edge_reason, edge_conf))));
                    }
                }
                if direction == &Direction::Up {
                    continue;
                }
                // Falls through to Downstream for Both.
                let out_start = graph.out_offsets[curr_idx].to_native() as usize;
                let out_end = graph.out_offsets[curr_idx + 1].to_native() as usize;
                for i in out_start..out_end {
                    let edge = &graph.edges[i];
                    let edge_conf = edge.confidence.to_native();
                    if edge_conf < min_conf {
                        hidden_edges += 1;
                        continue;
                    }
                    if let Some(rels) = rel_filter.as_ref() {
                        let rel_str = rel_to_str(&edge.rel_type);
                        if !rels.iter().any(|r| r == rel_str) {
                            continue;
                        }
                    }
                    let next_idx = edge.target.to_native() as usize;
                    if !visited.contains(&next_idx) {
                        visited.insert(next_idx);
                        let edge_reason = edge.reason.resolve(&graph.string_pool).to_string();
                        queue.push_back((next_idx, curr_depth + 1, Some((edge_reason, edge_conf))));
                    }
                }
            }
            Direction::Down => {
                let out_start = graph.out_offsets[curr_idx].to_native() as usize;
                let out_end = graph.out_offsets[curr_idx + 1].to_native() as usize;
                for i in out_start..out_end {
                    let edge = &graph.edges[i];
                    let edge_conf = edge.confidence.to_native();
                    if edge_conf < min_conf {
                        hidden_edges += 1;
                        continue;
                    }
                    if let Some(rels) = rel_filter.as_ref() {
                        let rel_str = rel_to_str(&edge.rel_type);
                        if !rels.iter().any(|r| r == rel_str) {
                            continue;
                        }
                    }
                    let next_idx = edge.target.to_native() as usize;
                    if !visited.contains(&next_idx) {
                        visited.insert(next_idx);
                        let edge_reason = edge.reason.resolve(&graph.string_pool).to_string();
                        queue.push_back((next_idx, curr_depth + 1, Some((edge_reason, edge_conf))));
                    }
                }
            }
        }
    }

    (results, hidden_edges)
}

fn collect_blind_spots(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    target_file_path: &str,
) -> Vec<String> {
    graph
        .blind_spots
        .iter()
        .filter(|bs| bs.file_path.resolve(&graph.string_pool) == target_file_path)
        .map(|bs| bs.kind.resolve(&graph.string_pool).to_string())
        .collect()
}

/// Map `NodeKind` (live) to the same strings used in the graph.
fn node_kind_to_str(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::File => "File",
        NodeKind::Function => "Function",
        NodeKind::Class => "Class",
        NodeKind::Method => "Method",
        NodeKind::Interface => "Interface",
        NodeKind::Constructor => "Constructor",
        NodeKind::Property => "Property",
        NodeKind::Variable => "Variable",
        NodeKind::Const => "Const",
        NodeKind::Import => "Import",
        NodeKind::Route => "Route",
        NodeKind::Process => "Process",
        NodeKind::Document => "Document",
        NodeKind::Section => "Section",
        NodeKind::EntryPoint => "EntryPoint",
        NodeKind::Struct => "Struct",
        NodeKind::Enum => "Enum",
        NodeKind::Typedef => "Typedef",
        NodeKind::Namespace => "Namespace",
        NodeKind::Module => "Module",
        NodeKind::Macro => "Macro",
        NodeKind::Annotation => "Annotation",
        NodeKind::Trait => "Trait",
        NodeKind::Impl => "Impl",
    }
}

/// FNV-64 hash of the source lines spanning [start_row, end_row] (inclusive,
/// 0-based). Normalises trailing whitespace so indent-only edits are stable.
fn hash_node_lines(lines: &[&[u8]], start_row: u32, end_row: u32) -> u64 {
    const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
    const FNV_PRIME: u64 = 1_099_511_628_211;

    let start = start_row as usize;
    let end = (end_row as usize).min(lines.len().saturating_sub(1));
    if start > end || start >= lines.len() {
        return 0;
    }

    let mut hash = FNV_OFFSET;
    for &line in &lines[start..=end] {
        let trimmed = line
            .iter()
            .rposition(|&b| b != b' ' && b != b'\t' && b != b'\r')
            .map(|pos| &line[..=pos])
            .unwrap_or(b"");
        for &byte in trimmed {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash ^= b'\n' as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Fetch the content of a repo-relative path at a specific git ref via
/// `git show <ref>:<path>`. Returns `None` for paths not present at that ref.
fn head_blob_at(repo: &std::path::Path, rel_path: &str, git_ref: &str) -> Option<Vec<u8>> {
    use crate::git::safe_exec;
    let out = safe_exec::git()
        .args(["show", &format!("{git_ref}:{rel_path}")])
        .current_dir(repo)
        .output()
        .ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}
