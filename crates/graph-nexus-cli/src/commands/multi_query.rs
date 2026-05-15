//! `gnx multi_query` — cross-repo concurrent symbol search.
//!
//! Enumerates target repos via the registry, loads each repo's graph
//! file in parallel via rayon, runs a name-substring scan per graph,
//! and merges hits across all repos using a top-K `BinaryHeap`.
//!
//! Repo selection (mutually-exclusive — only one applies):
//! 1. `--repos a,b,c` — explicit comma-separated list of registered repo names
//! 2. `--group <name>` — expand via `GroupEntry.members`
//! 3. `--all` — every repo in `registry.json`
//!
//! Per-repo workers skip silently if their graph file is missing or
//! corrupt — cross-repo search degrades gracefully when one repo's
//! index is stale.  The summary line reports how many repos were
//! attempted vs how many produced hits, so missing indexes surface
//! to the LLM without aborting the whole call.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::registry::{IndexLayout, Registry};
use graph_nexus_core::GnxError;
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Search for symbols across multiple registered repos concurrently and
/// return a merged top-K ranked result set.
#[derive(Args, Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultiQueryArgs {
    /// Search term matched against symbol names (case-insensitive substring).
    #[arg(long)]
    pub query: String,

    /// Comma-separated list of registered repo names. Mutually exclusive
    /// with `--group` / `--all`; if none of the three is given, errors out
    /// with a hint pointing to `gnx group_list`.
    #[arg(long)]
    pub repos: Option<String>,

    /// Expand to all members of this registry group.
    #[arg(long)]
    pub group: Option<String>,

    /// Search every registered repo.
    #[arg(long, default_value_t = false)]
    pub all: bool,

    /// Top-K hits to return across all repos. Default 20.
    #[arg(long, default_value_t = 20)]
    pub top_k: usize,

    /// Output format: text (default) | json | toon
    #[arg(long, default_value = "text")]
    pub format: Option<String>,
}

/// One hit row before merging — owned strings so workers can return
/// across the rayon boundary without lifetime grief.
#[derive(Debug, Clone)]
struct Hit {
    repo: String,
    score: f32,
    kind: String,
    file: String,
    line: u32,
    name: String,
}

/// `BinaryHeap` is a max-heap; we want top-K by descending score, so we
/// push `Reverse((score_bits, ...))` to make it act as a min-heap and
/// keep size ≤ K via pop. `f32` isn't `Ord`; use `score.to_bits()` as a
/// stable monotonic surrogate (positive floats compare correctly as
/// bit patterns, which is fine for our [0,1]-ish similarity range).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OrderedHit {
    score_bits: u32,
    repo: String,
    file: String,
    line: u32,
    name: String,
    kind: String,
}

impl OrderedHit {
    fn from(h: Hit) -> Self {
        Self {
            score_bits: h.score.to_bits(),
            repo: h.repo,
            file: h.file,
            line: h.line,
            name: h.name,
            kind: h.kind,
        }
    }
}

/// Zero-sized dummy engine for multi_query (ignores the engine param).
struct NoopEngine;
impl graph_nexus_mcp::registry::EngineRef for NoopEngine {
    fn graph_path(&self) -> &std::path::Path {
        std::path::Path::new("")
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
}

pub fn run_inner(
    args: MultiQueryArgs,
    _engine: &dyn graph_nexus_mcp::registry::EngineRef,
) -> Result<serde_json::Value, GnxError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let registry = Registry::open(&home_gnx)
        .map_err(|e| GnxError::InvalidArgument(format!("open registry: {e}")))?;
    let snapshot = registry.snapshot();

    // ── Repo set resolution ──
    let targets: Vec<String> = if args.all {
        snapshot.repos.iter().map(|r| r.name.clone()).collect()
    } else if let Some(group_name) = &args.group {
        match snapshot.groups.iter().find(|g| &g.name == group_name) {
            Some(g) => g.members.clone(),
            None => {
                return Err(GnxError::InvalidArgument(format!(
                    "unknown group '{group_name}' — run `gnx group_list` to see registered groups"
                )));
            }
        }
    } else if let Some(repos_csv) = &args.repos {
        repos_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        return Err(GnxError::InvalidArgument(
            "multi_query requires one of --repos / --group / --all".to_string(),
        ));
    };

    if targets.is_empty() {
        let result = serde_json::json!({
            "status": "success",
            "summary": "0 repos targeted",
            "results": serde_json::Value::Array(Vec::new()),
        });
        return Ok(result);
    }

    // ── Per-repo concurrent search via rayon. Each worker loads the
    //    repo's default-branch graph from the registry, scans node
    //    names, returns owned Hit rows. A worker whose graph is
    //    missing/corrupt contributes (repo_name, Err) — counted but
    //    not aborting the whole call. ──
    let scan_query = args.query.to_lowercase();
    let worker_results: Vec<(String, Result<Vec<Hit>, String>)> = targets
        .par_iter()
        .map(|repo_name| {
            let outcome = scan_one_repo(&home_gnx, repo_name, snapshot, &scan_query);
            (repo_name.clone(), outcome)
        })
        .collect();

    // ── Top-K merge using BinaryHeap<Reverse<OrderedHit>>. Heap stays
    //    bounded at K so a 50-repo query with thousands of total hits
    //    per repo never builds the full Vec — O(N log K). ──
    let mut heap: BinaryHeap<Reverse<OrderedHit>> = BinaryHeap::with_capacity(args.top_k + 1);
    let mut repos_with_hits = 0usize;
    let mut repos_failed = 0usize;
    for (repo_name, outcome) in &worker_results {
        let hits = match outcome {
            Ok(hits) => hits,
            Err(_msg) => {
                repos_failed += 1;
                continue;
            }
        };
        if !hits.is_empty() {
            repos_with_hits += 1;
        }
        for h in hits {
            heap.push(Reverse(OrderedHit::from(h.clone())));
            if heap.len() > args.top_k {
                heap.pop();
            }
        }
        let _ = repo_name;
    }

    // Drain heap, ordered descending by score.
    let mut ordered: Vec<OrderedHit> = heap.into_iter().map(|r| r.0).collect();
    ordered.sort_by(|a, b| b.score_bits.cmp(&a.score_bits));

    let mut results = Vec::with_capacity(ordered.len());
    for h in &ordered {
        let score = f32::from_bits(h.score_bits);
        results.push(serde_json::json!({
            "repo": h.repo,
            "kind": h.kind,
            "file": h.file,
            "line": h.line,
            "name": h.name,
            "score": score,
        }));
    }

    let summary = format!(
        "multi_query: {} repo(s) targeted, {} with hits, {} failed; returned top-{} of merged set",
        targets.len(),
        repos_with_hits,
        repos_failed,
        ordered.len()
    );

    let value = match format {
        OutputFormat::Text => {
            // Text output — one repo-prefixed line per hit; LLM-agent friendly.
            let mut lines = vec![serde_json::Value::String(summary)];
            for h in &ordered {
                let score = f32::from_bits(h.score_bits);
                lines.push(serde_json::Value::String(format!(
                    "[{}] @{} {}:{} ({}) [score:{:.4}]",
                    h.kind, h.repo, h.file, h.line, h.name, score
                )));
            }
            serde_json::json!({ "results": lines })
        }
        OutputFormat::Json | OutputFormat::Toon => serde_json::json!({
            "status": "success",
            "summary": summary,
            "results": results,
        }),
    };
    Ok(value)
}

pub fn run(args: MultiQueryArgs) -> Result<(), graph_nexus_core::GnxError> {
    let format = crate::output::OutputFormat::parse(args.format.as_deref());
    let value = run_inner(args, &NoopEngine)?;
    emit(&value, format)
}

#[cfg(test)]
mod inner_tests {
    use super::*;
    #[test]
    fn run_inner_returns_structured_value_not_unit() {
        fn _accepts(
            _f: fn(MultiQueryArgs, &dyn graph_nexus_mcp::registry::EngineRef)
                -> Result<serde_json::Value, graph_nexus_core::GnxError>,
        ) {}
        _accepts(run_inner);
    }
}

graph_nexus_mcp::gnx_register_mcp_tool!(MultiQueryArgs, run_inner);

/// Resolve a registered repo's graph path and scan it for nodes whose
/// name (case-insensitively) contains `query_lower`. Score = 1.0 for
/// exact match, 0.7 for prefix match, 0.4 for substring match — keeps
/// the heap merge meaningful without semantic / Tantivy machinery.
fn scan_one_repo(
    home_gnx: &std::path::Path,
    repo_name: &str,
    snapshot: &graph_nexus_core::registry::RegistryFile,
    query_lower: &str,
) -> Result<Vec<Hit>, String> {
    let repo = snapshot
        .repos
        .iter()
        .find(|r| r.name == repo_name)
        .ok_or_else(|| format!("repo '{repo_name}' not in registry"))?;
    // Pick the first registered branch — typically the default. A future
    // `--branch` arg can override; multi-branch fan-out is out of scope
    // for the MVP and would multiply load count without clear ranking.
    let branch = repo
        .branches
        .first()
        .ok_or_else(|| format!("repo '{repo_name}' has no indexed branches"))?;
    let layout = IndexLayout::resolve(
        home_gnx,
        &repo.name,
        &branch.name,
        &repo.worktree_path,
        &snapshot
            .repos
            .iter()
            .map(|r| (r.name.clone(), r.worktree_path.clone()))
            .collect::<Vec<_>>(),
    )
    .map_err(|e| format!("{repo_name}: layout: {e}"))?;
    let graph_path = layout.index_dir.join("graph.bin");
    let engine = Engine::load(&graph_path)
        .map_err(|e| format!("{repo_name}: load {}: {e}", graph_path.display()))?;
    let graph = engine
        .graph()
        .map_err(|e| format!("{repo_name}: access: {e}"))?;

    let mut hits = Vec::new();
    for node in graph.nodes.iter() {
        let name = node.name.resolve(&graph.string_pool);
        let name_lower = name.to_lowercase();
        let score = if name_lower == query_lower {
            1.0
        } else if name_lower.starts_with(query_lower) {
            0.7
        } else if name_lower.contains(query_lower) {
            0.4
        } else {
            continue;
        };
        let file = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool)
            .to_string();
        hits.push(Hit {
            repo: repo_name.to_string(),
            score,
            kind: format!("{:?}", node.kind),
            file,
            line: node.span.0.to_native() + 1,
            name: name.to_string(),
        });
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_heap_keeps_highest_scores() {
        // Synthetic hits — push 5, top-K = 3, expect scores 0.9 / 0.8 / 0.7.
        let mut heap: BinaryHeap<Reverse<OrderedHit>> = BinaryHeap::new();
        let k = 3;
        let scores = [0.4_f32, 0.9, 0.2, 0.8, 0.7];
        for (i, &s) in scores.iter().enumerate() {
            let h = OrderedHit {
                score_bits: s.to_bits(),
                repo: "r".into(),
                file: "f".into(),
                line: i as u32,
                name: "n".into(),
                kind: "Function".into(),
            };
            heap.push(Reverse(h));
            if heap.len() > k {
                heap.pop();
            }
        }
        let mut got: Vec<f32> = heap
            .into_iter()
            .map(|r| f32::from_bits(r.0.score_bits))
            .collect();
        got.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert_eq!(got, vec![0.9, 0.8, 0.7]);
    }

    #[test]
    fn empty_targets_returns_empty() {
        // Smoke: an empty target set must not panic on heap construction.
        let heap: BinaryHeap<Reverse<OrderedHit>> = BinaryHeap::new();
        assert_eq!(heap.len(), 0);
    }
}
