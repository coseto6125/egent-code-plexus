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
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum Direction {
    Up,
    Down,
    Both,
}

#[derive(Args, Debug)]
pub struct ImpactArgs {
    /// Target symbol name (mutually exclusive with --since).
    pub name: Option<String>,

    /// Git ref — compute blast radius across all symbols changed since this
    /// ref. Mutually exclusive with positional <name>.
    #[arg(long, conflicts_with = "name")]
    pub since: Option<String>,

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

    /// Default ON — only follow confidence ≥ 0.8 edges (framework-aware
    /// references). Override with `--high-trust-only=false` to walk all.
    #[arg(long, alias = "high_trust_only", default_value_t = true, action = clap::ArgAction::Set)]
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
    #[arg(long, default_value = "toon")]
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

pub fn run(args: ImpactArgs, engine: &Engine) -> Result<(), GnxError> {
    match (args.name.as_ref(), args.since.as_ref()) {
        (Some(_), None) => impact_by_name(args, engine),
        (None, Some(_)) => impact_since(args, engine),
        (None, None) => Err(GnxError::InvalidArgument(
            "impact requires either <name> positional or --since <ref>".into(),
        )),
        (Some(_), Some(_)) => unreachable!("clap conflicts_with prevents this"),
    }
}

fn impact_by_name(args: ImpactArgs, engine: &Engine) -> Result<(), GnxError> {
    let name = args.name.as_deref().unwrap();
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

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
        let result = json!({
            "error": format!("No symbol named '{name}' found in graph"),
            "hint": "Try `gnx search --query <name>` to find candidates, or check --file / --kind filters"
        });
        return emit(&result, format);
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
        let result = json!({
            "error": format!("'{name}' is ambiguous ({} candidates) — add --file or --kind to disambiguate", matches.len()),
            "candidates": candidates,
        });
        return emit(&result, format);
    }

    let min_conf = resolve_min_conf(&args);
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());

    let mut all_results: Vec<Value> = Vec::new();
    for start_idx in &matches {
        let bfs_result = run_bfs(
            graph,
            *start_idx,
            &args.direction,
            args.depth,
            min_conf,
            args.include_tests,
            &rel_filter,
        );
        all_results.extend(bfs_result);
    }

    // Empty callers hint for upstream direction.
    let first_start = matches[0];
    let impact_without_start: Vec<&Value> = all_results
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
        .collect();
    let emit_empty_hint = impact_without_start.is_empty() && args.direction == Direction::Up;

    let target_file_idx = graph.nodes[first_start].file_idx.to_native() as usize;
    let target_file_path = graph.files[target_file_idx]
        .path
        .resolve(&graph.string_pool)
        .to_string();
    let blind_spot_kinds = collect_blind_spots(graph, &target_file_path);

    let mut result_obj = json!({
        "status": "success",
        "target": name,
        "direction": direction_str(&args.direction),
        "impact": all_results,
    });

    if !blind_spot_kinds.is_empty() {
        let mut by_kind = std::collections::BTreeMap::<String, u32>::new();
        for k in &blind_spot_kinds {
            *by_kind.entry(k.clone()).or_insert(0) += 1;
        }
        result_obj["blind_spot_warning"] = json!({
            "file": target_file_path,
            "total": blind_spot_kinds.len(),
            "by_kind": by_kind,
            "note": "traversal may be incomplete — see `gnx doctor` blind spots catalog",
        });
    }

    emit(&result_obj, format)?;

    if emit_empty_hint {
        eprintln!(
            "→ \"{name}\" exists but has 0 incoming references. Possible: entry point, dead code, or recent rename. Try --direction both / --include-tests"
        );
    }

    Ok(())
}

fn impact_since(args: ImpactArgs, engine: &Engine) -> Result<(), GnxError> {
    let since_ref = args.since.as_deref().unwrap();
    let repo_path = PathBuf::from(args.repo.as_deref().unwrap_or("."));
    let format = OutputFormat::parse(args.format.as_deref());

    let scope = DiffScope::Compare(since_ref.to_string());
    let provider = ShellGitProvider;
    let file_diffs = provider.diff(&repo_path, &scope)?;

    if file_diffs.is_empty() {
        let result = json!({
            "status": "success",
            "since": since_ref,
            "message": "0 changes detected — no symbols to assess",
            "changed_symbols": [],
            "impact_by_symbol": [],
        });
        return emit(&result, format);
    }

    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    // Identify changed file paths from the diff.
    let changed_paths: Vec<String> = file_diffs
        .iter()
        .filter(|fd| args.include_tests || !is_test_path(&fd.file_path))
        .map(|fd| fd.file_path.clone())
        .collect();

    // Re-parse new side to detect changed symbols.
    let pipeline = make_pipeline();
    let mut new_map: HashMap<(String, String, String), (u64, u32)> = HashMap::new();
    let mut old_map: HashMap<(String, String, String), u64> = HashMap::new();

    for rel_path in &changed_paths {
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
                        let kind_str = node_kind_to_str(&raw.kind).to_string();
                        new_map.insert((kind_str, rel_path.clone(), raw.name.clone()), (h, raw.span.0));
                    }
                }
            }
        }

        if let Some(old_src) = head_blob_at(&repo_path, rel_path, since_ref) {
            let rel_pb = PathBuf::from(rel_path);
            if let Ok(lg) = pipeline.parse_file_raw(&rel_pb, &old_src) {
                let lines: Vec<&[u8]> = old_src.split(|&b| b == b'\n').collect();
                for raw in &lg.nodes {
                    if matches!(raw.kind, NodeKind::File | NodeKind::Process) {
                        continue;
                    }
                    let h = hash_node_lines(&lines, raw.span.0, raw.span.2);
                    let kind_str = node_kind_to_str(&raw.kind).to_string();
                    old_map.insert((kind_str, rel_path.clone(), raw.name.clone()), h);
                }
            }
        }
    }

    // Build lookup from old graph: (kind_str, file_path, name) → node_idx.
    let changed_files_set: HashSet<&str> = changed_paths.iter().map(|s| s.as_str()).collect();
    let mut old_graph_idx: HashMap<(String, String, String), usize> = HashMap::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        let file_node = &graph.files[node.file_idx.to_native() as usize];
        let file_path = file_node.path.resolve(&graph.string_pool);
        if !changed_files_set.contains(file_path) {
            continue;
        }
        let kind_str = kind_to_str(&node.kind).to_string();
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
    for &start_idx in &changed_node_indices {
        let node = &graph.nodes[start_idx];
        let sym_name = node.name.resolve(&graph.string_pool).to_string();
        let sym_file = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool)
            .to_string();
        let bfs_result = run_bfs(
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
    }

    let result = json!({
        "status": "success",
        "since": since_ref,
        "changed_symbols": changed_symbols,
        "impact_by_symbol": impact_by_symbol,
    });
    emit(&result, format)
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
/// Returns a flat Vec of JSON objects (one per visited node). The start node
/// appears at depth 0. `--include-tests` / `--relation-types` / `min_conf` are
/// applied here; `--kind` / `--file` emission-only filtering is NOT applied
/// here (callers can filter the returned Vec if needed).
fn run_bfs(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    start_idx: usize,
    direction: &Direction,
    max_depth: usize,
    min_conf: f32,
    include_tests: bool,
    rel_filter: &Option<Vec<String>>,
) -> Vec<Value> {
    type ViaEdge = Option<(String, f32)>;
    type Step = (usize, usize, ViaEdge);

    let mut visited = HashSet::new();
    let mut queue: VecDeque<Step> = VecDeque::new();
    let mut results = Vec::new();
    let mut test_path_cache = HashMap::new();

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

    results
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
