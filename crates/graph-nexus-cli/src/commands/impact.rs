use crate::commands::format::{kind_to_str, rel_to_str};
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::{Args, ValueEnum};
use graph_nexus_core::algorithms::process_trace::is_test_path;
use graph_nexus_core::config;
use graph_nexus_core::{GnxError, HIGH_TRUST_CONFIDENCE};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum Direction {
    Upstream,
    Downstream,
}

#[derive(Args, Debug)]
pub struct ImpactArgs {
    /// Target symbol UID
    #[arg(long)]
    pub target: String,

    /// Direction of traversal
    #[arg(long, value_enum)]
    pub direction: Direction,

    /// Maximum depth of traversal
    #[arg(long, default_value = "5")]
    pub depth: usize,

    #[arg(long)]
    pub repo: Option<String>,

    /// Output format
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,

    /// Skip edges with confidence < 0.8 (e.g. framework-aware refs like
    /// FastAPI `Depends()`, Axum/Express route handler bindings). Default
    /// off — all edges traversed.
    #[arg(long, alias = "high_trust_only", default_value_t = false)]
    pub high_trust_only: bool,

    /// Minimum confidence threshold (0.0 to 1.0) for edges to traverse. Overrides --high-trust-only if provided.
    #[arg(long, alias = "min_confidence")]
    pub min_confidence: Option<f32>,

    /// If false (default), nodes located in typical test files/directories are omitted from the output.
    #[arg(long, aliases = ["include_tests", "includeTests"], default_value_t = false)]
    pub include_tests: bool,

    /// Comma-separated node kinds to keep in the output (lowercase match
    /// against `kind_to_str`, e.g. `function,method`). Filter is applied at
    /// emission only — non-matching nodes still act as stepping stones so a
    /// matched descendant downstream remains reachable.
    #[arg(long)]
    pub kind: Option<String>,

    /// Substring filter on result entries by file path. Same emission-only
    /// semantics as `--kind` (non-matching nodes are still traversed).
    /// Snake-case is the documented form (CLAUDE.md GitNexus workflow); the
    /// kebab alias is accepted too.
    #[arg(long = "file_path", alias = "file-path")]
    pub file_path: Option<String>,

    /// Comma-separated relation types to follow during traversal (e.g.
    /// `calls,extends`). Edges of other rel types are skipped entirely,
    /// shrinking the BFS frontier rather than just filtering output.
    /// Snake-case is the documented form; kebab alias is accepted.
    #[arg(long = "relation_types", alias = "relation-types")]
    pub relation_types: Option<String>,
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
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    // Find the target node index by UID
    let mut start_idx = None;
    for (i, node) in graph.nodes.iter().enumerate() {
        if node.uid.resolve(&graph.string_pool) == args.target {
            start_idx = Some(i);
            break;
        }
    }

    let start_idx = match start_idx {
        Some(idx) => idx,
        None => {
            let result = serde_json::json!({
                "error": format!("Symbol UID '{}' not found", args.target)
            });
            return emit(&result, format);
        }
    };

    // BFS Traversal
    //
    // `via_edge` carries the (reason, confidence) of the edge that brought us
    // to this node, so each result entry can expose WHY it was reached. None
    // for the start node.
    type ViaEdge = Option<(String, f32)>;
    type Step = (usize, usize, ViaEdge);
    let mut visited = HashSet::new();
    let mut queue: VecDeque<Step> = VecDeque::new();
    let mut results = Vec::new();

    queue.push_back((start_idx, 0, None));
    visited.insert(start_idx);

    // Confidence threshold precedence: explicit --min-confidence > --high-trust-only
    // > repo-local `config.toml` confidence.high_trust_threshold > built-in const.
    // Loading config is best-effort — a missing / malformed TOML falls back to the
    // const so no command is ever bricked by a stale config file.
    let repo_root = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let cfg_threshold = config::load(&repo_root)
        .map(|c| c.confidence.high_trust_threshold)
        .unwrap_or(HIGH_TRUST_CONFIDENCE);
    let min_conf = args.min_confidence.unwrap_or(if args.high_trust_only {
        cfg_threshold
    } else {
        0.0
    });
    let mut test_path_cache = std::collections::HashMap::new();

    // Pre-parse filters once so the inner BFS loop stays a tight comparison.
    let kind_filter = parse_csv_lower(args.kind.as_deref());
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    let file_path_filter = args.file_path.as_deref();

    while let Some((curr_idx, curr_depth, via)) = queue.pop_front() {
        let curr_node = &graph.nodes[curr_idx];
        let file_idx = curr_node.file_idx.to_native() as usize;

        if !args.include_tests {
            let is_test = *test_path_cache.entry(file_idx).or_insert_with(|| {
                let file_node = &graph.files[file_idx];
                let file_path = file_node.path.resolve(&graph.string_pool);
                is_test_path(file_path)
            });
            if is_test {
                continue;
            }
        }

        let file_node = &graph.files[file_idx];
        let file_path = file_node.path.resolve(&graph.string_pool);

        let (via_reason, via_confidence) = via
            .as_ref()
            .map(|(r, c)| (r.as_str(), *c))
            .unwrap_or(("", 1.0));

        // --kind / --file_path are emission-only filters. A non-matching node
        // can still be a stepping stone whose descendants match, so we keep
        // walking past it but suppress the result entry. The start node is
        // exempt so callers always see the target in the output.
        let kind_str_lower = kind_to_str(&curr_node.kind).to_ascii_lowercase();
        let kind_ok = kind_filter
            .as_ref()
            .is_none_or(|k| k.iter().any(|w| w == &kind_str_lower));
        let path_ok = file_path_filter.is_none_or(|needle| file_path.contains(needle));

        if curr_idx == start_idx || (kind_ok && path_ok) {
            results.push(serde_json::json!({
                "uid": curr_node.uid.resolve(&graph.string_pool),
                "name": curr_node.name.resolve(&graph.string_pool),
                "kind": kind_to_str(&curr_node.kind),
                "filePath": file_path,
                "line": curr_node.span.0.to_native(),
                "depth": curr_depth,
                "viaReason": via_reason,
                "viaConfidence": via_confidence,
            }));
        }

        if curr_depth >= args.depth {
            continue;
        }

        match args.direction {
            Direction::Upstream => {
                let in_start = graph.in_offsets[curr_idx].to_native() as usize;
                let in_end = graph.in_offsets[curr_idx + 1].to_native() as usize;
                for i in in_start..in_end {
                    let edge_idx = graph.in_edge_idx[i].to_native() as usize;
                    let edge = &graph.edges[edge_idx];
                    let edge_conf = edge.confidence.to_native();
                    if edge_conf < min_conf {
                        continue;
                    }
                    // --relation_types narrows the BFS frontier itself, so an
                    // unmatched rel type prevents traversal (not just emission).
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
            }
            Direction::Downstream => {
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

    // Blind-spot warning: if the target's file contains any blind-spot site,
    // upstream/downstream traversal may be incomplete (eval / dynamic-import
    // calls can't be statically traced). Surfacing the kinds + count lets the
    // LLM know to widen its grep before trusting the impact result.
    let target_file_idx = graph.nodes[start_idx].file_idx.to_native() as usize;
    let target_file_path = graph.files[target_file_idx]
        .path
        .resolve(&graph.string_pool)
        .to_string();
    let blind_spot_kinds: Vec<String> = graph
        .blind_spots
        .iter()
        .filter(|bs| bs.file_path.resolve(&graph.string_pool) == target_file_path)
        .map(|bs| bs.kind.resolve(&graph.string_pool).to_string())
        .collect();

    let mut result_obj = serde_json::json!({
        "status": "success",
        "target": args.target,
        "direction": match args.direction {
            Direction::Upstream => "upstream",
            Direction::Downstream => "downstream",
        },
        "impact": results,
    });

    if !blind_spot_kinds.is_empty() {
        let mut by_kind = std::collections::BTreeMap::<String, u32>::new();
        for k in &blind_spot_kinds {
            *by_kind.entry(k.clone()).or_insert(0) += 1;
        }
        result_obj["blind_spot_warning"] = serde_json::json!({
            "file": target_file_path,
            "total": blind_spot_kinds.len(),
            "by_kind": by_kind,
            "note": "traversal may be incomplete — see `gnx doctor` blind spots catalog",
        });
    }

    emit(&result_obj, format)
}
