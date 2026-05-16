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
use std::collections::{BinaryHeap, HashMap};

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
    /// Up to `HOP_EXPANSION_LIMIT` 1-hop incoming-edge source names.
    /// Populated from `in_offsets` / `in_edge_idx` / `edges`. Empty when
    /// the node has no callers or all edges have been truncated.
    pub callers: Vec<String>,
    /// Up to `HOP_EXPANSION_LIMIT` 1-hop outgoing-edge target names.
    /// Populated from `out_offsets` / `edges`.
    pub callees: Vec<String>,
}

/// Cap per-direction. Matches the legacy gitnexus augmentation engine,
/// which sliced top 3 to keep hook context dense without blowing token
/// budget — empirically the 4th+ caller/callee adds little signal once
/// the LLM already has the symbol's file:line and kind.
const HOP_EXPANSION_LIMIT: usize = 3;

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
    callers: Vec<String>,
    callees: Vec<String>,
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
            callers: h.callers,
            callees: h.callees,
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
    let index_dir = engine.index_dir();

    let effective_mode = match mode {
        SearchMode::Auto => detect_mode(pattern, embeddings_available_for(graph)),
        m => m.clone(),
    };

    let kind_set: Option<Vec<String>> =
        kind_filter.map(|s| s.split(',').map(|k| k.trim().to_lowercase()).collect());

    let mut hits = match effective_mode {
        SearchMode::Bm25 | SearchMode::Auto => {
            bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
        }
        SearchMode::Vector => {
            vector_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
        }
        SearchMode::Hybrid => {
            hybrid_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
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

/// Primary BM25 path: queries the persisted Tantivy index when present,
/// falling back to a per-name substring scan (exact 1.0 / prefix 0.7 /
/// substring 0.4) when `<index_dir>/tantivy/` is missing — which happens
/// on a freshly-cloned repo before `gnx admin index` has run.
fn bm25_hits_from_graph(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: Option<&std::path::Path>,
) -> Vec<Hit> {
    if let Some(dir) = index_dir {
        if dir.join("tantivy").exists() {
            return tantivy_hits(graph, pattern, kind_set, repo_label, dir);
        }
    }
    substring_hits(graph, pattern, kind_set, repo_label)
}

/// Query the on-disk Tantivy BM25 index, map uids back to graph nodes,
/// and materialise `Hit` rows. Returns an empty vec when the index opens
/// but yields no matches; falls through to substring scan if the query
/// fails outright (e.g. corrupt segment), preserving the contract that
/// hooks never error out on search.
fn tantivy_hits(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: &std::path::Path,
) -> Vec<Hit> {
    let scored = match crate::search::TantivyEngine::search(index_dir, pattern) {
        Some(s) => s,
        // Index unavailable / corrupt / parse error — fall through so
        // hook context isn't silently empty.
        None => return substring_hits(graph, pattern, kind_set, repo_label),
    };
    // Index ran cleanly. An empty scored vec means BM25 ruled out every
    // symbol; we MUST NOT fall back to substring scan, since that would
    // surface 0.4-scored noise the trusted index already rejected.
    if scored.is_empty() {
        return Vec::new();
    }

    // uid → node_idx lookup over the whole graph (capacity matches the
    // number of insertions, not the smaller `scored` set).
    let mut uid_to_idx: HashMap<&str, usize> = HashMap::with_capacity(graph.nodes.len());
    for (idx, node) in graph.nodes.iter().enumerate() {
        uid_to_idx.insert(node.uid.resolve(&graph.string_pool), idx);
    }

    let mut hits = Vec::with_capacity(scored.len());
    for (score, uid) in scored {
        let Some(&idx) = uid_to_idx.get(uid.as_str()) else {
            continue;
        };
        if let Some(hit) = build_hit(graph, idx, score, kind_set, repo_label) {
            hits.push(hit);
        }
    }
    hits
}

/// Fallback BM25-shaped scan when no tantivy index is on disk.
/// Preserves the legacy 1.0 / 0.7 / 0.4 scoring so hook output stays
/// shaped the same before the first `gnx admin index` has produced an
/// index.
fn substring_hits(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
) -> Vec<Hit> {
    let pattern_lower = pattern.to_lowercase();
    let mut hits = Vec::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        let name_lower = node.name.resolve(&graph.string_pool).to_lowercase();
        let score: f32 = if name_lower == pattern_lower {
            1.0
        } else if name_lower.starts_with(&pattern_lower) {
            0.7
        } else if name_lower.contains(&pattern_lower) {
            0.4
        } else {
            continue;
        };
        if let Some(hit) = build_hit(graph, idx, score, kind_set, repo_label) {
            hits.push(hit);
        }
    }
    hits
}

/// Shared per-node Hit constructor. Applies kind filter and reads
/// file/line/kind/caller_count from the archived graph. Returns `None`
/// when the node's kind doesn't match the filter.
fn build_hit(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    idx: usize,
    score: f32,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
) -> Option<Hit> {
    let node = &graph.nodes[idx];
    if let Some(ks) = kind_set {
        let node_kind_str = format!("{:?}", node.kind).to_lowercase();
        if !ks.iter().any(|k| k == &node_kind_str) {
            return None;
        }
    }
    let name = node.name.resolve(&graph.string_pool);
    let file = graph.files[node.file_idx.to_native() as usize]
        .path
        .resolve(&graph.string_pool)
        .to_string();
    let line = node.span.0.to_native() + 1;
    let kind_str = kind_to_str(&node.kind).to_string();
    let signature = format!("{kind_str} {name}");

    let in_start = graph.in_offsets[idx].to_native() as usize;
    let in_end = graph.in_offsets[idx + 1].to_native() as usize;
    let caller_count = in_end.saturating_sub(in_start);
    let callers: Vec<String> = graph.in_edge_idx[in_start..in_end]
        .iter()
        .take(HOP_EXPANSION_LIMIT)
        .map(|eidx| {
            let e = &graph.edges[eidx.to_native() as usize];
            graph.nodes[e.source.to_native() as usize]
                .name
                .resolve(&graph.string_pool)
                .to_string()
        })
        .collect();

    let out_start = graph.out_offsets[idx].to_native() as usize;
    let out_end = graph.out_offsets[idx + 1].to_native() as usize;
    let callees: Vec<String> = graph.edges[out_start..out_end]
        .iter()
        .take(HOP_EXPANSION_LIMIT)
        .map(|e| {
            graph.nodes[e.target.to_native() as usize]
                .name
                .resolve(&graph.string_pool)
                .to_string()
        })
        .collect();

    Some(Hit {
        repo: repo_label.clone(),
        score,
        kind: kind_str,
        file,
        line,
        name: name.to_string(),
        signature,
        caller_count,
        callers,
        callees,
    })
}

// ── Vector scoring primitives ────────────────────────────────────────────────

/// Plain L2 norm. Returns 0.0 for empty input or an all-zero vector.
pub(crate) fn l2_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Score `embeddings[i]` against `query` via cosine similarity, drop
/// zero-norm and non-positive entries, return the top-`k` as
/// `(node_idx, similarity)` sorted descending. Skip-marker zero
/// embeddings produced at build time get filtered here.
pub(crate) fn cosine_top_k_indices(
    embeddings: &[Vec<f32>],
    query: &[f32],
    k: usize,
) -> Vec<(usize, f32)> {
    let q_norm = l2_norm(query);
    if q_norm == 0.0 {
        return Vec::new();
    }

    let scored: Vec<(usize, f32)> = embeddings
        .par_iter()
        .enumerate()
        .filter_map(|(idx, emb)| {
            let dot: f32 = emb.iter().zip(query.iter()).map(|(a, b)| a * b).sum();
            let denom = l2_norm(emb) * q_norm;
            if denom == 0.0 {
                return None;
            }
            let sim = dot / denom;
            (sim > 0.0).then_some((idx, sim))
        })
        .collect();

    let mut heap: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::with_capacity(k + 1);
    for (idx, sim) in scored {
        heap.push(Reverse((sim.to_bits(), idx)));
        if heap.len() > k {
            heap.pop();
        }
    }

    let mut out: Vec<(usize, f32)> = heap
        .into_iter()
        .map(|r| (r.0 .1, f32::from_bits(r.0 .0)))
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Vector path: embed the query, score every node embedding by cosine,
/// materialise `Hit` rows for the top-K survivors. All failure modes
/// degrade to BM25 + a stderr hint — the hook contract requires that
/// search NEVER errors out.
fn vector_hits_from_graph(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: Option<&std::path::Path>,
) -> Vec<Hit> {
    let Some(archived_embs) = graph.embeddings.as_ref() else {
        eprintln!(
            "→ vector: graph has no embeddings — falling back to bm25 (rebuild with `gnx admin index --embeddings`)"
        );
        return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    };

    let embedder = match crate::embedder::get_embedder() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("→ vector: embedder unavailable ({e}) — falling back to bm25");
            return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
        }
    };

    let query_vec = match embedder.embed(vec![pattern.to_string()]) {
        Ok(mut vs) if !vs.is_empty() => vs.swap_remove(0),
        _ => {
            eprintln!("→ vector: query embed failed — falling back to bm25");
            return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
        }
    };

    tracing::debug!(query_norm = l2_norm(&query_vec), "vector query embedded");

    // Materialise archived → owned so the rayon-based ranking helper
    // (which expects `&[Vec<f32>]`) sees a contiguous slice. Cost:
    // ~5k × 1024 × 4 B ≈ 20 MB per call; allocator-light vs the
    // model inference that just happened.
    let owned_embs: Vec<Vec<f32>> = archived_embs
        .iter()
        .map(|v| v.iter().map(|x| x.to_native()).collect())
        .collect();

    let ranked = cosine_top_k_indices(&owned_embs, &query_vec, TOP_K);

    ranked
        .into_iter()
        .filter_map(|(idx, score)| build_hit(graph, idx, score, kind_set, repo_label))
        .collect()
}

/// Hybrid path: run BM25 and vector, then fuse via RRF. Short-circuits
/// to BM25 when the graph has no embeddings — vector_hits_from_graph
/// would also do this, but skipping the duplicate BM25 call saves a
/// Tantivy round-trip when we know the vector half can't contribute.
fn hybrid_hits_from_graph(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: Option<&std::path::Path>,
) -> Vec<Hit> {
    if graph.embeddings.is_none() {
        eprintln!(
            "→ hybrid: graph has no embeddings — falling back to bm25 (rebuild with `gnx admin index --embeddings`)"
        );
        return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    }

    let bm25 = bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    let vec = vector_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    rrf_merge(bm25, vec)
}

/// Reciprocal Rank Fusion constant. k=60 is the Cormack et al. 2009
/// default — the parameter used by Elasticsearch / Vespa / Weaviate
/// for hybrid retrieval. Hard-wired; add a flag if we ever need to
/// tune per query type.
const RRF_K: f32 = 60.0;

/// Fuse two ranked `Vec<Hit>` lists by Reciprocal Rank Fusion:
/// `score(uid) = Σ 1/(RRF_K + rank_i + 1)` over the lists containing
/// `uid`. Output sorted descending by combined score, truncated to
/// `TOP_K`. The merged Hit's `score` field is overwritten with the
/// RRF score so emit/serialise layers see the fused number.
///
/// Dedup key: `(file, line, name)`. Stable within a single graph —
/// the only context this helper runs in. Multi-repo merge happens
/// later in `compute_multi`, which keys on the full `OrderedHit`
/// including `repo`.
pub(crate) fn rrf_merge(bm25: Vec<Hit>, vec: Vec<Hit>) -> Vec<Hit> {
    type Key = (String, u32, String);
    let key = |h: &Hit| -> Key { (h.file.clone(), h.line, h.name.clone()) };

    let mut scores: HashMap<Key, (f32, Hit)> = HashMap::new();

    for (rank, h) in bm25.into_iter().enumerate() {
        let s = 1.0 / (RRF_K + rank as f32 + 1.0);
        scores
            .entry(key(&h))
            .and_modify(|e| e.0 += s)
            .or_insert_with(|| (s, h));
    }
    for (rank, h) in vec.into_iter().enumerate() {
        let s = 1.0 / (RRF_K + rank as f32 + 1.0);
        scores
            .entry(key(&h))
            .and_modify(|e| e.0 += s)
            .or_insert_with(|| (s, h));
    }

    let mut combined: Vec<(f32, Hit)> = scores.into_values().collect();
    combined.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    combined
        .into_iter()
        .take(TOP_K)
        .map(|(score, mut h)| {
            h.score = score;
            h
        })
        .collect()
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
            callers: o.callers,
            callees: o.callees,
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
        compute_single(
            &args.pattern,
            &args.mode,
            args.kind.as_deref(),
            engine,
            None,
        )
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
    let index_dir = engine.index_dir();

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
        index_dir,
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
        // Text-format must show *something* — an empty `results` array
        // renders as a blank line on stdout, which agents can't tell apart
        // from "command crashed silently". Surface the same hint json/toon
        // already include.
        let hint =
            "No matches found. Try a shorter pattern or `gnx search --mode bm25 <fragment>`.";
        if matches!(format, OutputFormat::Text) {
            return emit(
                &serde_json::json!({ "results": [serde_json::Value::String(hint.into())] }),
                format,
            );
        }
        return emit(
            &serde_json::json!({
                "status": "success",
                "results": [],
                "hint": hint,
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
                callers: vec![],
                callees: vec![],
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

    #[test]
    fn l2_norm_handles_zero_vec() {
        assert_eq!(super::l2_norm(&[]), 0.0);
        assert_eq!(super::l2_norm(&[0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn l2_norm_unit_vec_is_one() {
        let v = [1.0_f32 / 3.0_f32.sqrt(); 3];
        assert!((super::l2_norm(&v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_top_k_indices_ranks_by_similarity() {
        let embs = vec![
            vec![1.0, 0.0, 0.0], // node 0 — orthogonal
            vec![0.0, 1.0, 0.0], // node 1 — identical direction
            vec![0.0, 0.7, 0.7], // node 2 — partially aligned
            vec![0.0, 0.0, 0.0], // node 3 — skip-marker
        ];
        let query = vec![0.0, 1.0, 0.0];
        let ranked = super::cosine_top_k_indices(&embs, &query, 3);
        assert_eq!(ranked[0].0, 1, "node 1 should rank first");
        assert_eq!(ranked[1].0, 2, "node 2 should rank second");
        assert!(ranked.iter().all(|(idx, _)| *idx != 3));
    }

    fn make_test_hit(name: &str, file: &str, line: u32, score: f32) -> super::Hit {
        super::Hit {
            repo: None,
            score,
            kind: "function".into(),
            file: file.into(),
            line,
            name: name.into(),
            signature: format!("function {name}"),
            caller_count: 0,
            callers: vec![],
            callees: vec![],
        }
    }

    #[test]
    fn rrf_merge_combines_two_ranked_lists() {
        let bm25 = vec![
            make_test_hit("A", "a.rs", 1, 10.0),
            make_test_hit("B", "b.rs", 2, 8.0),
            make_test_hit("C", "c.rs", 3, 6.0),
        ];
        let vec = vec![
            make_test_hit("B", "b.rs", 2, 0.9),
            make_test_hit("A", "a.rs", 1, 0.8),
            make_test_hit("D", "d.rs", 4, 0.7),
        ];
        let merged = super::rrf_merge(bm25, vec);
        // A and B appear in both lists → expected to take the top 2 slots.
        let top_names: Vec<&str> = merged.iter().take(2).map(|h| h.name.as_str()).collect();
        assert!(top_names.contains(&"A") && top_names.contains(&"B"));
        assert_eq!(merged.len(), 4);
    }

    #[test]
    fn rrf_merge_dedupes_by_file_line_name() {
        let bm25 = vec![make_test_hit("A", "a.rs", 1, 5.0)];
        let vec = vec![make_test_hit("A", "a.rs", 1, 0.9)];
        let merged = super::rrf_merge(bm25, vec);
        assert_eq!(merged.len(), 1);
        // 1/(60+1) + 1/(60+1) = 2/61
        assert!((merged[0].score - (2.0 / 61.0)).abs() < 1e-6);
    }

    #[test]
    fn rrf_merge_truncates_to_top_k() {
        let bm25: Vec<super::Hit> = (0..30)
            .map(|i| make_test_hit(&format!("n{i}"), "x.rs", i, 1.0))
            .collect();
        let merged = super::rrf_merge(bm25, vec![]);
        assert_eq!(merged.len(), super::TOP_K);
    }
}
