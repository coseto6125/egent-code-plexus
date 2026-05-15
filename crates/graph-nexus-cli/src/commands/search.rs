//! `gnx search` — unified symbol search with hybrid modes + cross-repo fan-out.
//!
//! Replaces `query` (single-repo BM25/vector) and absorbs `multi_query`
//! (cross-repo rayon fan-out + top-K heap merge).
//!
//! ## Mode routing
//! - `bm25`   — pure lexical (substring scoring against graph node names)
//! - `vector` — semantic cosine similarity (stub; falls back to bm25 until
//!   full embedding wiring is complete — TODO: wire to real embed path)
//! - `hybrid` — bm25 + vector folded (stub: falls back to bm25 without embeddings)
//! - `auto`   — detect: slug-like input → bm25; else → hybrid if embeddings
//!   present, else bm25 with a stderr hint
//!
//! ## Cross-repo fan-out
//! When `--repo` resolves to multiple repos, workers run in parallel via
//! rayon and hits are merged via a top-K BinaryHeap (port from multi_query).

use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::{Args, ValueEnum};
use graph_nexus_core::registry::{IndexLayout, Registry};
use graph_nexus_core::GnxError;
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

const TOP_K: usize = 20;

// ── Public API ───────────────────────────────────────────────────────────────

#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum SearchMode {
    Bm25,
    Vector,
    Hybrid,
    Auto,
}

#[derive(Args, Debug, Clone)]
pub struct SearchArgs {
    /// Pattern: name fragment or natural-language description.
    pub pattern: String,

    /// Search mode: bm25 (lexical), vector (semantic), hybrid (combined), auto (detect).
    #[arg(long, value_enum, default_value_t = SearchMode::Auto)]
    pub mode: SearchMode,

    /// Filter by node kinds (csv: function,method,class,...).
    #[arg(long)]
    pub kind: Option<String>,

    /// Repository selector (path | name | @group | @all | csv mix). Defaults to cwd.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: text (default) | json | toon.
    #[arg(long, default_value = "text")]
    pub format: Option<String>,
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run(args: SearchArgs, engine: &Engine) -> Result<(), GnxError> {
    let format = OutputFormat::parse(args.format.as_deref());

    // Resolve --repo to a list of targets.  When the selector is absent or
    // expands to exactly one cwd-repo, use the already-loaded `engine` rather
    // than re-loading.  When it expands to multiple repos, fan out via rayon.
    let targets = resolve_targets(args.repo.as_deref())?;

    if targets.is_empty() {
        // Single-repo path using the engine that main.rs already loaded.
        run_single(args.pattern, args.mode, args.kind, format, engine, None)
    } else if targets.len() == 1 {
        let (repo_name, graph_path) = targets.into_iter().next().unwrap();
        let local_engine = Engine::load(std::path::PathBuf::from(&graph_path))
            .map_err(|e| GnxError::Rkyv(format!("{repo_name}: load: {e}")))?;
        run_single(
            args.pattern,
            args.mode,
            args.kind,
            format,
            &local_engine,
            Some(repo_name),
        )
    } else {
        run_multi(args.pattern, args.mode, args.kind, format, targets)
    }
}

// ── Mode detection ───────────────────────────────────────────────────────────

fn detect_mode(input: &str, embeddings_available: bool) -> SearchMode {
    let slug_like =
        !input.is_empty() && input.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if slug_like {
        return SearchMode::Bm25;
    }
    if embeddings_available {
        SearchMode::Hybrid
    } else {
        eprintln!(
            "→ falling back to bm25 (no embeddings — build with `gnx admin index --embeddings`)"
        );
        SearchMode::Bm25
    }
}

/// Stub: returns true only when the graph has an embeddings table.
/// TODO: query per-repo BranchEntry.embedding_status from the registry.
fn embeddings_available_for(graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph) -> bool {
    graph.embeddings.is_some()
}

// ── Per-repo hit struct ───────────────────────────────────────────────────────

/// One result row — owned strings so rayon workers can return across threads.
#[derive(Debug, Clone)]
pub struct Hit {
    pub repo: Option<String>,
    pub score: f32,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub name: String,
    pub signature: String,
    pub caller_count: usize,
}

/// `BinaryHeap` key that is `Ord`.  f32 isn't `Ord`; use `score_bits` as a
/// monotonic surrogate (positive floats compare correctly as bit patterns in
/// [0,1]-ish range).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OrderedHit {
    score_bits: u32,
    repo: Option<String>,
    file: String,
    line: u32,
    name: String,
    kind: String,
    signature: String,
    caller_count: usize,
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
            signature: h.signature,
            caller_count: h.caller_count,
        }
    }
}

// ── Single-repo search ────────────────────────────────────────────────────────

fn run_single(
    pattern: String,
    mode: SearchMode,
    kind_filter: Option<String>,
    format: OutputFormat,
    engine: &Engine,
    repo_label: Option<String>,
) -> Result<(), GnxError> {
    let hits = compute_single(&pattern, &mode, kind_filter.as_deref(), engine, repo_label)?;
    emit_hits(&hits, format, None)
}

/// Pure compute path for single-repo search: returns owned Hit rows top-K trimmed.
fn compute_single(
    pattern: &str,
    mode: &SearchMode,
    kind_filter: Option<&str>,
    engine: &Engine,
    repo_label: Option<String>,
) -> Result<Vec<Hit>, GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    let effective_mode = match mode {
        SearchMode::Auto => detect_mode(pattern, embeddings_available_for(graph)),
        m => m.clone(),
    };

    let kind_set: Option<Vec<String>> =
        kind_filter.map(|s| s.split(',').map(|k| k.trim().to_lowercase()).collect());

    let mut hits = match effective_mode {
        SearchMode::Bm25 | SearchMode::Auto => {
            bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label)
        }
        SearchMode::Vector => {
            // TODO: wire to real cosine path (graph_nexus_analyzer::embeddings)
            eprintln!("→ vector mode not yet wired — falling back to bm25");
            bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label)
        }
        SearchMode::Hybrid => {
            // TODO: fold bm25 + cosine scores when embeddings are wired
            eprintln!("→ hybrid: embeddings not wired — using bm25");
            bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label)
        }
    };

    // Sort by score descending, trim to TOP_K.
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(TOP_K);

    Ok(hits)
}

/// BM25 scan directly against the graph nodes: exact → 1.0, prefix → 0.7, substring → 0.4.
fn bm25_hits_from_graph(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
) -> Vec<Hit> {
    let pattern_lower = pattern.to_lowercase();
    let mut hits = Vec::new();

    for (idx, node) in graph.nodes.iter().enumerate() {
        let name = node.name.resolve(&graph.string_pool);
        let name_lower = name.to_lowercase();
        let score: f32 = if name_lower == pattern_lower {
            1.0
        } else if name_lower.starts_with(&pattern_lower) {
            0.7
        } else if name_lower.contains(&pattern_lower) {
            0.4
        } else {
            continue;
        };

        // Apply --kind filter.
        if let Some(ks) = kind_set {
            let node_kind_str = format!("{:?}", node.kind).to_lowercase();
            if !ks.iter().any(|k| k == &node_kind_str) {
                continue;
            }
        }

        let file = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool)
            .to_string();
        let line = node.span.0.to_native() + 1;
        let kind_str = kind_to_str(&node.kind).to_string();
        let signature = format!("{kind_str} {name}");

        // 1-hop caller count: in-edge CSR slice length for this node.
        let caller_count = {
            let start = graph.in_offsets[idx].to_native() as usize;
            let end = graph.in_offsets[idx + 1].to_native() as usize;
            end.saturating_sub(start)
        };

        hits.push(Hit {
            repo: repo_label.clone(),
            score,
            kind: kind_str,
            file,
            line,
            name: name.to_string(),
            signature,
            caller_count,
        });
    }

    hits
}

// ── Multi-repo fan-out ────────────────────────────────────────────────────────

fn run_multi(
    pattern: String,
    mode: SearchMode,
    kind_filter: Option<String>,
    format: OutputFormat,
    targets: Vec<(String, String)>, // (repo_name, graph_path_str)
) -> Result<(), GnxError> {
    let (hits, summary) = compute_multi(&pattern, &mode, kind_filter.as_deref(), targets)?;
    emit_hits(&hits, format, Some(summary))
}

/// Pure compute path for multi-repo fan-out. Returns merged top-K hits + summary string.
fn compute_multi(
    pattern: &str,
    mode: &SearchMode,
    kind_filter: Option<&str>,
    targets: Vec<(String, String)>, // (repo_name, graph_path_str)
) -> Result<(Vec<Hit>, String), GnxError> {
    let kind_set: Option<Vec<String>> =
        kind_filter.map(|s| s.split(',').map(|k| k.trim().to_lowercase()).collect());

    // Fan out via rayon; workers return owned hit rows.
    let worker_results: Vec<(String, Result<Vec<Hit>, String>)> = targets
        .par_iter()
        .map(|(repo_name, graph_path)| {
            let outcome = scan_repo(repo_name, graph_path, pattern, &kind_set, mode);
            (repo_name.clone(), outcome)
        })
        .collect();

    // Top-K merge using BinaryHeap<Reverse<OrderedHit>> — O(N log K).
    let mut heap: BinaryHeap<Reverse<OrderedHit>> = BinaryHeap::with_capacity(TOP_K + 1);
    let mut repos_with_hits = 0usize;
    let mut repos_failed = 0usize;

    for (_repo_name, outcome) in &worker_results {
        match outcome {
            Err(_) => repos_failed += 1,
            Ok(hits) => {
                if !hits.is_empty() {
                    repos_with_hits += 1;
                }
                for h in hits {
                    heap.push(Reverse(OrderedHit::from(h.clone())));
                    if heap.len() > TOP_K {
                        heap.pop();
                    }
                }
            }
        }
    }

    // Drain heap in descending score order.
    let mut ordered: Vec<OrderedHit> = heap.into_iter().map(|r| r.0).collect();
    ordered.sort_by(|a, b| b.score_bits.cmp(&a.score_bits));

    let hits: Vec<Hit> = ordered
        .into_iter()
        .map(|o| Hit {
            repo: o.repo,
            score: f32::from_bits(o.score_bits),
            kind: o.kind,
            file: o.file,
            line: o.line,
            name: o.name,
            signature: o.signature,
            caller_count: o.caller_count,
        })
        .collect();

    let summary = format!(
        "search: {} repo(s) targeted, {} with hits, {} failed; returned top-{} of merged set",
        targets.len(),
        repos_with_hits,
        repos_failed,
        hits.len()
    );

    Ok((hits, summary))
}

/// In-process search entry point for hooks and other internal consumers.
/// Returns owned `Hit` rows without going through stdout / OutputFormat.
/// Top-K trimmed identically to `run`.
pub fn compute_hits(args: SearchArgs, engine: &Engine) -> Result<Vec<Hit>, GnxError> {
    let targets = resolve_targets(args.repo.as_deref())?;
    if targets.is_empty() {
        compute_single(&args.pattern, &args.mode, args.kind.as_deref(), engine, None)
    } else if targets.len() == 1 {
        let (repo_name, graph_path) = targets.into_iter().next().unwrap();
        let local_engine = Engine::load(std::path::PathBuf::from(&graph_path))
            .map_err(|e| GnxError::Rkyv(format!("{repo_name}: load: {e}")))?;
        compute_single(
            &args.pattern,
            &args.mode,
            args.kind.as_deref(),
            &local_engine,
            Some(repo_name),
        )
    } else {
        compute_multi(&args.pattern, &args.mode, args.kind.as_deref(), targets)
            .map(|(hits, _summary)| hits)
    }
}

/// Load one repo's graph and scan it (used by rayon workers).
fn scan_repo(
    repo_name: &str,
    graph_path: &str,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    mode: &SearchMode,
) -> Result<Vec<Hit>, String> {
    let engine = Engine::load(std::path::PathBuf::from(graph_path))
        .map_err(|e| format!("{repo_name}: load {graph_path}: {e}"))?;
    let graph = engine
        .graph()
        .map_err(|e| format!("{repo_name}: access: {e}"))?;

    let effective_mode = match mode {
        SearchMode::Auto => detect_mode(pattern, embeddings_available_for(graph)),
        m => m.clone(),
    };

    // All modes except a real vector path fall through to bm25 for now.
    // TODO: wire vector/hybrid to graph_nexus_analyzer::embeddings.
    let _ = effective_mode;
    Ok(bm25_hits_from_graph(
        graph,
        pattern,
        kind_set,
        &Some(repo_name.to_string()),
    ))
}

// ── Repo selector resolution ─────────────────────────────────────────────────

/// Resolve `--repo` to `Vec<(name, graph_path_str)>`.
/// Returns empty Vec when the selector is absent (caller uses pre-loaded engine).
fn resolve_targets(selector: Option<&str>) -> Result<Vec<(String, String)>, GnxError> {
    let sel = match selector {
        None | Some(".") | Some("") => return Ok(vec![]),
        Some(s) => s,
    };

    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let registry = Registry::open(&home_gnx)
        .map_err(|e| GnxError::InvalidArgument(format!("open registry: {e}")))?;
    let snapshot = registry.snapshot();

    let repo_names: Vec<String> = if sel == "@all" {
        snapshot.repos.iter().map(|r| r.name.clone()).collect()
    } else if let Some(group_name) = sel.strip_prefix('@') {
        match snapshot.groups.iter().find(|g| g.name == group_name) {
            Some(g) => g.members.clone(),
            None => {
                return Err(GnxError::InvalidArgument(format!(
                    "unknown group '{group_name}' — run `gnx admin group list` to see registered groups"
                )));
            }
        }
    } else {
        sel.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    if repo_names.is_empty() {
        return Ok(vec![]);
    }

    let name_path_pairs: Vec<(String, String)> = snapshot
        .repos
        .iter()
        .map(|r| (r.name.clone(), r.worktree_path.clone()))
        .collect();

    let mut targets = Vec::with_capacity(repo_names.len());
    for repo_name in &repo_names {
        let repo = match snapshot.repos.iter().find(|r| &r.name == repo_name) {
            Some(r) => r,
            None => continue, // silently skip unknown names
        };
        let branch = match repo.branches.first() {
            Some(b) => b,
            None => continue,
        };
        let layout = IndexLayout::resolve(
            &home_gnx,
            &repo.name,
            &branch.name,
            &repo.worktree_path,
            &name_path_pairs,
        )
        .map_err(|e| GnxError::InvalidArgument(format!("{repo_name}: layout: {e}")))?;
        let graph_path = layout.index_dir.join("graph.bin");
        targets.push((repo_name.clone(), graph_path.to_string_lossy().into_owned()));
    }

    Ok(targets)
}

// ── Emission ──────────────────────────────────────────────────────────────────

fn emit_hits(hits: &[Hit], format: OutputFormat, summary: Option<String>) -> Result<(), GnxError> {
    if hits.is_empty() {
        return emit(
            &serde_json::json!({
                "status": "success",
                "results": [],
                "hint": "No matches found. Try a shorter pattern or `gnx search --mode bm25 <fragment>`.",
            }),
            format,
        );
    }

    match format {
        OutputFormat::Text => {
            let mut lines: Vec<serde_json::Value> = Vec::new();
            if let Some(s) = &summary {
                lines.push(serde_json::Value::String(s.clone()));
            }
            for h in hits {
                let repo_prefix = h
                    .repo
                    .as_deref()
                    .map(|r| format!("@{r} "))
                    .unwrap_or_default();
                lines.push(serde_json::Value::String(format!(
                    "[{}] {}{}:{} ({}) callers:{} [score:{:.4}]",
                    h.kind, repo_prefix, h.file, h.line, h.name, h.caller_count, h.score,
                )));
            }
            emit(&serde_json::json!({ "results": lines }), format)
        }
        OutputFormat::Json | OutputFormat::Toon => {
            let results: Vec<serde_json::Value> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "repo": h.repo,
                        "name": h.name,
                        "kind": h.kind,
                        "file": h.file,
                        "line": h.line,
                        "signature": h.signature,
                        "caller_count": h.caller_count,
                        "score": h.score,
                    })
                })
                .collect();
            let mut payload = serde_json::json!({
                "status": "success",
                "results": results,
            });
            if let Some(s) = summary {
                payload["summary"] = serde_json::Value::String(s);
            }
            emit(&payload, format)
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_mode_slug_routes_to_bm25() {
        assert_eq!(detect_mode("validateUser", false), SearchMode::Bm25);
        assert_eq!(detect_mode("foo_bar_123", false), SearchMode::Bm25);
    }

    #[test]
    fn detect_mode_phrase_routes_to_hybrid_when_embeddings() {
        assert_eq!(
            detect_mode("how does authentication work", true),
            SearchMode::Hybrid
        );
    }

    #[test]
    fn detect_mode_phrase_falls_back_to_bm25_without_embeddings() {
        assert_eq!(
            detect_mode("how does authentication work", false),
            SearchMode::Bm25
        );
    }

    #[test]
    fn top_k_heap_keeps_highest_scores() {
        let mut heap: BinaryHeap<Reverse<OrderedHit>> = BinaryHeap::new();
        let k = 3;
        let scores = [0.4_f32, 0.9, 0.2, 0.8, 0.7];
        for (i, &s) in scores.iter().enumerate() {
            let h = OrderedHit {
                score_bits: s.to_bits(),
                repo: None,
                file: "f".into(),
                line: i as u32,
                name: "n".into(),
                kind: "Function".into(),
                signature: "fn n".into(),
                caller_count: 0,
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
    fn compute_hits_signature_check() {
        fn _check(_: fn(SearchArgs, &Engine) -> Result<Vec<Hit>, GnxError>) {}
        _check(compute_hits);
    }
}
