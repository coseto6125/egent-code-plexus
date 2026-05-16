//! `gnx search` — BM25 symbol search with cross-repo fan-out.
//!
//! Replaces `query` (single-repo BM25) and absorbs `multi_query`
//! (cross-repo rayon fan-out + top-K heap merge).
//!
//! BM25 is served by the persisted tantivy index when `<index_dir>/tantivy/`
//! exists; otherwise the substring scan fallback runs against the archived
//! graph so a freshly cloned repo still produces shaped output before the
//! first `gnx admin index` has materialised the lexical index.
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
}

#[derive(Args, Debug, Clone)]
pub struct SearchArgs {
    /// Pattern: name fragment or natural-language description. Required
    /// unless `--batch` is set (in which case patterns come from stdin).
    #[arg(required_unless_present = "batch")]
    pub pattern: Option<String>,

    /// Search mode (only `bm25` is supported; flag retained as an
    /// explicit no-op alias so existing scripts / hooks that pass
    /// `--mode bm25` keep working without a deprecation warning).
    #[arg(long, value_enum, default_value_t = SearchMode::Bm25)]
    pub mode: SearchMode,

    /// Filter by node kinds (csv: function,method,class,...).
    #[arg(long)]
    pub kind: Option<String>,

    /// Repository selector (path | name | @group | @all | csv mix). Defaults to cwd.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: text (default) | json | toon.
    #[arg(long)]
    pub format: Option<String>,

    /// Read patterns from stdin (one per line, lines starting with `#`
    /// or empty are skipped). Engines are loaded once outside the
    /// per-query loop so mmap setup + rkyv access are amortised across
    /// queries. Each query is emitted as a separate block prefixed by
    /// `=== pattern: <pattern> ===`.
    #[arg(long)]
    pub batch: bool,
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run(args: SearchArgs, engine: &Engine) -> Result<(), GnxError> {
    if args.batch {
        return run_batch(args, engine);
    }

    let format = OutputFormat::parse(args.format.as_deref());
    let pattern = args.pattern.clone().ok_or_else(|| {
        GnxError::InvalidArgument("pattern is required (or use --batch to read from stdin)".into())
    })?;

    // Resolve --repo to a list of targets.  When the selector is absent or
    // expands to exactly one cwd-repo, use the already-loaded `engine` rather
    // than re-loading.  When it expands to multiple repos, fan out via rayon.
    let targets = resolve_targets(args.repo.as_deref())?;

    if targets.is_empty() {
        // Single-repo path using the engine that main.rs already loaded.
        run_single(pattern, args.mode, args.kind, format, engine, None)
    } else if targets.len() == 1 {
        let (repo_name, graph_path) = targets.into_iter().next().unwrap();
        let local_engine = Engine::load(std::path::PathBuf::from(&graph_path))
            .map_err(|e| GnxError::Rkyv(format!("{repo_name}: load: {e}")))?;
        run_single(
            pattern,
            args.mode,
            args.kind,
            format,
            &local_engine,
            Some(repo_name),
        )
    } else {
        run_multi(pattern, args.mode, args.kind, format, targets)
    }
}

/// Batch dispatch: read patterns from stdin, one query at a time.
///
/// Output: each query block is preceded by a `=== pattern: <pattern> ===`
/// stdout line so scripts can split per-query regardless of `--format`.
/// Engine instances are loaded once outside the per-query loop
/// (single-repo: one Engine; multi-repo: one per target via
/// `load_engines_lossy`) so mmap setup + rkyv access are amortised
/// across queries. Per-repo load failures in multi-repo mode degrade
/// to 0 hits + failure count rather than killing the batch.
fn run_batch(args: SearchArgs, engine: &Engine) -> Result<(), GnxError> {
    use std::io::BufRead;

    let format = OutputFormat::parse(args.format.as_deref());
    let targets = resolve_targets(args.repo.as_deref())?;

    let stdin = std::io::stdin();
    let queries: Vec<String> = stdin
        .lock()
        .lines()
        .map_while(Result::ok)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .collect();

    if queries.is_empty() {
        eprintln!("→ batch: no patterns on stdin (one per line, `#` for comments)");
        return Ok(());
    }

    let single_repo_engine: Option<(String, Engine)> = if targets.len() == 1 {
        let (repo_name, graph_path) = &targets[0];
        let eng = Engine::load(std::path::PathBuf::from(graph_path))
            .map_err(|e| GnxError::InvalidArgument(format!("{repo_name}: load: {e}")))?;
        Some((repo_name.clone(), eng))
    } else {
        None
    };
    let multi_repo_engines: Option<Vec<(String, Result<Engine, String>)>> = if targets.len() > 1 {
        Some(load_engines_lossy(&targets))
    } else {
        None
    };

    for pattern in &queries {
        println!("=== pattern: {pattern} ===");

        let hits = if targets.is_empty() {
            compute_single(pattern, &args.mode, args.kind.as_deref(), engine, None)?
        } else if let Some((repo_name, local_engine)) = single_repo_engine.as_ref() {
            compute_single(
                pattern,
                &args.mode,
                args.kind.as_deref(),
                local_engine,
                Some(repo_name.clone()),
            )?
        } else {
            let loaded = multi_repo_engines.as_ref().unwrap();
            let (hits, _summary) =
                compute_multi_with_engines(pattern, &args.mode, args.kind.as_deref(), loaded);
            hits
        };

        emit_hits(&hits, format, None)?;
    }
    Ok(())
}

// ── Per-repo hit struct ───────────────────────────────────────────────────────

/// Origin of `Hit.score` — annotates which ranker produced the value
/// so downstream consumers (the LLM, tests, scripts) can tell a tantivy
/// BM25 score apart from a fallback substring score without inferring
/// it from the magnitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScoreSource {
    /// Tantivy BM25 (term frequency × IDF × field length norm).
    Bm25,
    /// Hardcoded substring buckets (1.0 exact / 0.7 prefix / 0.4 contains)
    /// — emitted when `<index_dir>/tantivy/` is missing.
    Substring,
}

impl ScoreSource {
    /// Wire-format tag used in JSON / Toon `score_source` field and the
    /// text-format `[score:N source:X]` suffix.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bm25 => "bm25",
            Self::Substring => "substring",
        }
    }
}

/// One result row — owned strings so rayon workers can return across threads.
#[derive(Debug, Clone)]
pub struct Hit {
    pub repo: Option<String>,
    pub score: f32,
    /// Which ranker produced `score`. Annotation only — does not change
    /// the score value or the sort order.
    pub score_source: ScoreSource,
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
    score_source: ScoreSource,
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
            score_source: h.score_source,
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

    let kind_set: Option<Vec<String>> =
        kind_filter.map(|s| s.split(',').map(|k| k.trim().to_lowercase()).collect());

    let _ = mode;
    let mut hits = bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir);

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
        if let Some(hit) = build_hit(graph, idx, score, ScoreSource::Bm25, kind_set, repo_label) {
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
        if let Some(hit) = build_hit(
            graph,
            idx,
            score,
            ScoreSource::Substring,
            kind_set,
            repo_label,
        ) {
            hits.push(hit);
        }
    }
    hits
}

/// Shared per-node Hit constructor. Applies kind filter and reads
/// file/line/kind/caller_count from the archived graph. Returns `None`
/// when the node's kind doesn't match the filter. `score_source`
/// annotates which ranker produced `score` (BM25 / substring).
fn build_hit(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    idx: usize,
    score: f32,
    score_source: ScoreSource,
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
        score_source,
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

/// Pre-load engines for a batch of target repos. Each engine load is
/// captured as a per-repo `Result<Engine, String>` so individual
/// failures don't kill the whole multi-repo query — the failing repo
/// contributes 0 hits and is counted in the summary.
pub fn load_engines_lossy(targets: &[(String, String)]) -> Vec<(String, Result<Engine, String>)> {
    targets
        .iter()
        .map(|(repo_name, graph_path)| {
            let result = Engine::load(std::path::PathBuf::from(graph_path))
                .map_err(|e| format!("load {graph_path}: {e}"));
            (repo_name.clone(), result)
        })
        .collect()
}

/// Fan out across pre-loaded engines via rayon, score each repo, then
/// merge to a global top-K. Exposed (vs the thin `compute_multi`
/// wrapper) so batch callers can pay the `Engine::load` cost once
/// across N queries instead of N × M times.
pub fn compute_multi_with_engines(
    pattern: &str,
    mode: &SearchMode,
    kind_filter: Option<&str>,
    loaded: &[(String, Result<Engine, String>)],
) -> (Vec<Hit>, String) {
    let kind_set: Option<Vec<String>> =
        kind_filter.map(|s| s.split(',').map(|k| k.trim().to_lowercase()).collect());

    // Fan out via rayon; workers return owned hit rows.
    let _ = mode;
    let worker_results: Vec<(String, Result<Vec<Hit>, String>)> = loaded
        .par_iter()
        .map(|(repo_name, engine_result)| {
            let outcome = match engine_result {
                Err(e) => Err(format!("{repo_name}: {e}")),
                Ok(engine) => engine
                    .graph()
                    .map_err(|e| format!("{repo_name}: access: {e}"))
                    .map(|graph| {
                        let repo_label = Some(repo_name.clone());
                        bm25_hits_from_graph(
                            graph,
                            pattern,
                            &kind_set,
                            &repo_label,
                            engine.index_dir(),
                        )
                    }),
            };
            (repo_name.clone(), outcome)
        })
        .collect();

    // Top-K merge using BinaryHeap<Reverse<OrderedHit>> — O(N log K).
    let mut heap: BinaryHeap<Reverse<OrderedHit>> = BinaryHeap::with_capacity(TOP_K + 1);
    let mut repos_with_hits = 0usize;
    let mut repos_failed = 0usize;

    for (_repo_name, outcome) in worker_results {
        match outcome {
            Err(_) => repos_failed += 1,
            Ok(hits) => {
                if !hits.is_empty() {
                    repos_with_hits += 1;
                }
                for h in hits {
                    heap.push(Reverse(OrderedHit::from(h)));
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
            score_source: o.score_source,
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
        loaded.len(),
        repos_with_hits,
        repos_failed,
        hits.len()
    );

    (hits, summary)
}

/// Pure compute path for multi-repo fan-out. Loads all engines up-front
/// then delegates to `compute_multi_with_engines`. Single-shot callers
/// (single query × multi-repo) use this directly; batch callers should
/// pre-load engines themselves and call `compute_multi_with_engines`
/// inside the per-query loop to avoid reloading per query.
fn compute_multi(
    pattern: &str,
    mode: &SearchMode,
    kind_filter: Option<&str>,
    targets: Vec<(String, String)>, // (repo_name, graph_path_str)
) -> Result<(Vec<Hit>, String), GnxError> {
    let loaded = load_engines_lossy(&targets);
    Ok(compute_multi_with_engines(
        pattern,
        mode,
        kind_filter,
        &loaded,
    ))
}

/// In-process search entry point for hooks and other internal consumers.
/// Returns owned `Hit` rows without going through stdout / OutputFormat.
/// Top-K trimmed identically to `run`. Batch mode is not exposed here
/// — hooks always run one pattern at a time.
pub fn compute_hits(args: SearchArgs, engine: &Engine) -> Result<Vec<Hit>, GnxError> {
    let pattern = args.pattern.as_deref().ok_or_else(|| {
        GnxError::InvalidArgument("compute_hits requires a pattern (--batch not supported)".into())
    })?;
    let targets = resolve_targets(args.repo.as_deref())?;
    if targets.is_empty() {
        compute_single(pattern, &args.mode, args.kind.as_deref(), engine, None)
    } else if targets.len() == 1 {
        let (repo_name, graph_path) = targets.into_iter().next().unwrap();
        let local_engine = Engine::load(std::path::PathBuf::from(&graph_path))
            .map_err(|e| GnxError::Rkyv(format!("{repo_name}: load: {e}")))?;
        compute_single(
            pattern,
            &args.mode,
            args.kind.as_deref(),
            &local_engine,
            Some(repo_name),
        )
    } else {
        compute_multi(pattern, &args.mode, args.kind.as_deref(), targets)
            .map(|(hits, _summary)| hits)
    }
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
                    "[{}] {}{}:{} ({}) callers:{} [score:{:.4} source:{}]",
                    h.kind,
                    repo_prefix,
                    h.file,
                    h.line,
                    h.name,
                    h.caller_count,
                    h.score,
                    h.score_source.as_str(),
                )));
            }
            emit(&serde_json::json!({ "results": lines }), format)
        }
        OutputFormat::Json | OutputFormat::Toon | OutputFormat::Llm => {
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
                        "score_source": h.score_source.as_str(),
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
                score_source: ScoreSource::Bm25,
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
