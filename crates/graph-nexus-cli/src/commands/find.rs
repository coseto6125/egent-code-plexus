//! `gnx find` — unified symbol lookup. Three modes selectable via `--mode`:
//!
//! * `exact` (default) — exact-name match, single most-likely definition
//!   ranked by category priority (Source > Document > Config > Test) then
//!   caller count. Output: flat `{found, matches[], status}`. Designed
//!   for "where is X defined?" queries where the LLM already knows the
//!   exact symbol name.
//! * `fuzzy` — substring match with the same ranking + output shape as
//!   `exact`. `--fuzzy` is a shorthand that infers this mode.
//! * `bm25` — BM25 lexical ranking via the persisted tantivy index
//!   (substring-bucket fallback when no index is on disk). Output:
//!   five-bucket partition by `FileCategory` (`source` / `tests` /
//!   `reference` / `document` / `config`), each independently capped at
//!   `TOP_K` (20). For broad ranked discovery, not name-precise lookup.
//!
//! ## Cross-repo fan-out
//! When `--repo` resolves to multiple repos, BM25 mode workers run in
//! parallel via rayon and hits are merged via a top-K BinaryHeap.
//!
//! ## Batch (`--batch`)
//! BM25-mode only. Reads patterns from stdin (one per line, `#`
//! comments), loads engines once, emits one block per pattern prefixed by
//! `=== pattern: <pattern> ===`.
//!
//! BM25 is served by the persisted tantivy index when `<index_dir>/tantivy/`
//! exists; otherwise the substring scan fallback runs against the archived
//! graph so a freshly cloned repo still produces shaped output before the
//! first `gnx admin index` has materialised the lexical index. Every hit
//! carries a `language` field derived from file extension.

use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::{Args, ValueEnum};
use graph_nexus_analyzer::resolution::index::Language;
use graph_nexus_core::graph::{ArchivedFileCategory, ArchivedZeroCopyGraph, FileCategory};
use graph_nexus_core::registry::{resolve_home_gnx, Registry};
use graph_nexus_core::GnxError;
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

const TOP_K: usize = 20;

/// Raw-candidate cap before 5-way bucketing. With 5 categories each wanting
/// up to `TOP_K` items, fetch `TOP_K * 5` candidates so no bucket starves
/// when results cluster in fewer categories. Cap stays bounded — a query
/// matching thousands of names doesn't drag every node through ranking.
const MULTI_CAP: usize = TOP_K * 5;

// ── Public API ───────────────────────────────────────────────────────────────

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindMode {
    /// Exact-name match. Single most-likely definition by default;
    /// `--all` returns all exact matches.
    Exact,
    /// Substring match — same ranking + output shape as `exact`. Use
    /// when the precise name is unknown but a fragment is.
    Fuzzy,
    /// BM25 lexical ranking via tantivy. Bucketed top-K output.
    Bm25,
}

#[derive(Args, Debug, Clone)]
pub struct FindArgs {
    /// Pattern: symbol name (or name fragment in `fuzzy` / `bm25` mode).
    /// Required unless `--batch` is set (`bm25` mode only — patterns
    /// then come from stdin).
    #[arg(required_unless_present = "batch")]
    pub pattern: Option<String>,

    /// Lookup mode: `exact` (default), `fuzzy`, or `bm25`.
    #[arg(long, value_enum, default_value_t = FindMode::Exact)]
    pub mode: FindMode,

    /// Shorthand for `--mode fuzzy`. Ignored when `--mode` is supplied
    /// explicitly with a non-default value.
    #[arg(long)]
    pub fuzzy: bool,

    /// Return all matches instead of the single top-ranked one. Affects
    /// `exact` and `fuzzy` modes; `bm25` always returns top-K buckets.
    #[arg(long)]
    pub all: bool,

    /// Include hits from test files in `exact` / `fuzzy` modes
    /// (skipped by default). `bm25` mode bucketises into a separate
    /// `tests` array and is unaffected by this flag.
    #[arg(long)]
    pub include_tests: bool,

    /// Filter by node kinds (csv: function,method,class,...).
    #[arg(long)]
    pub kind: Option<String>,

    /// Repository selector (path | name | @group | @all | csv mix). Defaults to cwd.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: text (default) | json | toon.
    #[arg(long)]
    pub format: Option<String>,

    /// Read patterns from stdin (`bm25` mode only — one per line, lines
    /// starting with `#` or empty are skipped). Engines are loaded once
    /// outside the per-query loop so mmap setup + rkyv access are
    /// amortised across queries. Each query is emitted as a separate
    /// block prefixed by `=== pattern: <pattern> ===`.
    #[arg(long)]
    pub batch: bool,
}

impl FindArgs {
    /// `--fuzzy` infers `--mode fuzzy` only when `--mode` was left at the
    /// `Exact` default — explicit `--mode bm25 --fuzzy` keeps `bm25` so
    /// users can override the shorthand without rebuilding the struct.
    fn effective_mode(&self) -> FindMode {
        if self.fuzzy && self.mode == FindMode::Exact {
            FindMode::Fuzzy
        } else {
            self.mode
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run(args: FindArgs, engine: &Engine) -> Result<(), GnxError> {
    let mode = args.effective_mode();

    // --batch is BM25-only; reject it early in other modes so users see
    // the misconfiguration rather than silently falling back to a single
    // exact-mode query against an empty stdin.
    if args.batch && mode != FindMode::Bm25 {
        return Err(GnxError::InvalidArgument(
            "--batch is only supported with `--mode bm25`".into(),
        ));
    }

    match mode {
        FindMode::Exact | FindMode::Fuzzy => run_exact_or_fuzzy(args, engine, mode),
        FindMode::Bm25 => run_bm25(args, engine),
    }
}

fn run_bm25(args: FindArgs, engine: &Engine) -> Result<(), GnxError> {
    if args.batch {
        return run_batch(args, engine);
    }

    let format = OutputFormat::parse(args.format.as_deref());
    let pattern = args.pattern.clone().ok_or_else(|| {
        GnxError::InvalidArgument("pattern is required (or use --batch to read from stdin)".into())
    })?;

    let targets = resolve_targets(args.repo.as_deref())?;

    if targets.is_empty() {
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

// ── Exact / Fuzzy mode ───────────────────────────────────────────────────────

/// Flat per-match output for `exact` / `fuzzy` modes. Distinct from `Hit`
/// (which carries BM25-specific score / score_source / callers / callees)
/// so the JSON shape stays minimal — name + location + ranking signal —
/// matching the "definition lookup" question these modes answer.
#[derive(Debug, serde::Serialize)]
pub struct FindMatch {
    pub file: String,
    pub line: u32,
    pub name: String,
    pub kind: String,
    pub category: String,
    pub caller_count: u32,
    pub signature: String,
}

#[derive(Debug, serde::Serialize)]
pub struct FindResult {
    pub found: bool,
    pub matches: Vec<FindMatch>,
    pub status: String,
}

/// Category sort priority for exact/fuzzy ranking — lower is preferred.
/// `Reference` (vendored / third-party) gets the lowest priority because
/// these lookups are about the user's code, not their dependencies.
fn category_priority(cat: &ArchivedFileCategory) -> u8 {
    match cat {
        ArchivedFileCategory::Source => 0,
        ArchivedFileCategory::Document => 1,
        ArchivedFileCategory::Config => 2,
        ArchivedFileCategory::Test => 3,
        ArchivedFileCategory::Reference => 4,
    }
}

fn category_to_str(cat: &ArchivedFileCategory) -> &'static str {
    match cat {
        ArchivedFileCategory::Source => "Source",
        ArchivedFileCategory::Test => "Test",
        ArchivedFileCategory::Document => "Document",
        ArchivedFileCategory::Config => "Config",
        ArchivedFileCategory::Reference => "Reference",
    }
}

fn count_incoming(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> u32 {
    let in_start = graph.in_offsets[node_idx].to_native() as usize;
    let in_end = graph.in_offsets[node_idx + 1].to_native() as usize;
    (in_end - in_start) as u32
}

fn run_exact_or_fuzzy(args: FindArgs, engine: &Engine, mode: FindMode) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());
    let pattern = args.pattern.as_deref().ok_or_else(|| {
        GnxError::InvalidArgument("pattern is required in exact / fuzzy mode".into())
    })?;

    let kind_filter: Option<Vec<String>> = args.kind.as_deref().map(|s| {
        s.split(',')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .collect()
    });

    let mut candidates: Vec<(usize, u32, u8, String)> = graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(node_idx, node)| {
            let name = node.name.resolve(&graph.string_pool);
            let matches = match mode {
                FindMode::Exact => name == pattern,
                FindMode::Fuzzy => name.contains(pattern),
                FindMode::Bm25 => unreachable!("run_exact_or_fuzzy only handles Exact / Fuzzy"),
            };
            if !matches {
                return None;
            }

            if let Some(ref kinds) = kind_filter {
                let node_kind = kind_to_str(&node.kind).to_ascii_lowercase();
                if !kinds.iter().any(|k| k == &node_kind) {
                    return None;
                }
            }

            let file = &graph.files[node.file_idx.to_native() as usize];
            if !args.include_tests && matches!(file.category, ArchivedFileCategory::Test) {
                return None;
            }

            let prio = category_priority(&file.category);
            let caller_count = count_incoming(graph, node_idx);
            let file_path = file.path.resolve(&graph.string_pool).to_string();
            Some((node_idx, caller_count, prio, file_path))
        })
        .collect();

    // Sort: category priority asc, caller_count desc, file path asc.
    candidates.sort_unstable_by(|a, b| {
        a.2.cmp(&b.2)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.3.cmp(&b.3))
    });

    let selected: Vec<_> = if args.all {
        candidates
    } else {
        candidates.into_iter().take(1).collect()
    };

    let matches: Vec<FindMatch> = selected
        .into_iter()
        .map(|(node_idx, caller_count, _, _)| {
            let node = &graph.nodes[node_idx];
            let file = &graph.files[node.file_idx.to_native() as usize];
            FindMatch {
                file: file.path.resolve(&graph.string_pool).to_string(),
                line: node.span.0.to_native(),
                name: node.name.resolve(&graph.string_pool).to_string(),
                kind: kind_to_str(&node.kind).to_string(),
                category: category_to_str(&file.category).to_string(),
                caller_count,
                signature: node.uid.resolve(&graph.string_pool).to_string(),
            }
        })
        .collect();

    let found = !matches.is_empty();

    match format {
        OutputFormat::Text => {
            if !found {
                println!("no match for: {pattern}");
                return Ok(());
            }
            for m in &matches {
                let test_tag = if m.category == "Test" { " [test]" } else { "" };
                println!(
                    "[{}] {}:{}{} ({}) callers={}",
                    m.kind, m.file, m.line, test_tag, m.name, m.caller_count
                );
            }
            Ok(())
        }
        _ => {
            let result = FindResult {
                found,
                matches,
                status: "success".to_string(),
            };
            emit(
                &serde_json::to_value(&result).map_err(|e| GnxError::Output(e.to_string()))?,
                format,
            )
        }
    }
}

// ── BM25 batch dispatch ──────────────────────────────────────────────────────

/// Batch dispatch: read patterns from stdin, one query at a time.
///
/// Output: each query block is preceded by a `=== pattern: <pattern> ===`
/// stdout line so scripts can split per-query regardless of `--format`.
/// Engine instances are loaded once outside the per-query loop
/// (single-repo: one Engine; multi-repo: one per target via
/// `load_engines_lossy`) so mmap setup + rkyv access are amortised
/// across queries. Per-repo load failures in multi-repo mode degrade
/// to 0 hits + failure count rather than killing the batch.
fn run_batch(args: FindArgs, engine: &Engine) -> Result<(), GnxError> {
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

        let buckets = BucketedResults::partition(hits);
        emit_bucketed(&buckets, format, None)?;
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
    /// Language derived from file extension at output time (e.g. "Rust", "Python").
    pub language: String,
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
    /// File category used for bucket partitioning; not emitted to consumers.
    pub category: FileCategory,
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
    language: String,
    line: u32,
    name: String,
    kind: String,
    signature: String,
    caller_count: usize,
    callers: Vec<String>,
    callees: Vec<String>,
    score_source: ScoreSource,
    category: FileCategory,
}

impl OrderedHit {
    fn from(h: Hit) -> Self {
        Self {
            score_bits: h.score.to_bits(),
            repo: h.repo,
            file: h.file,
            language: h.language,
            line: h.line,
            name: h.name,
            kind: h.kind,
            signature: h.signature,
            caller_count: h.caller_count,
            callers: h.callers,
            callees: h.callees,
            score_source: h.score_source,
            category: h.category,
        }
    }
}

/// Five-bucket output — one per `FileCategory`. Empty buckets emit `[]` in
/// JSON and `(none)` in text; each bucket independently capped at `TOP_K`.
struct BucketedResults {
    source: Vec<Hit>,
    tests: Vec<Hit>,
    reference: Vec<Hit>,
    document: Vec<Hit>,
    config: Vec<Hit>,
}

impl BucketedResults {
    fn partition(mut hits: Vec<Hit>) -> Self {
        // Sort overall by descending score before partitioning so each bucket
        // gets the best representatives across repos.
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut source = Vec::new();
        let mut tests = Vec::new();
        let mut reference = Vec::new();
        let mut document = Vec::new();
        let mut config = Vec::new();

        for h in hits {
            let bucket = match h.category {
                FileCategory::Source => &mut source,
                FileCategory::Test => &mut tests,
                FileCategory::Reference => &mut reference,
                FileCategory::Document => &mut document,
                FileCategory::Config => &mut config,
            };
            if bucket.len() < TOP_K {
                bucket.push(h);
            }
        }

        Self {
            source,
            tests,
            reference,
            document,
            config,
        }
    }
}

// ── Single-repo search ────────────────────────────────────────────────────────

fn run_single(
    pattern: String,
    mode: FindMode,
    kind_filter: Option<String>,
    format: OutputFormat,
    engine: &Engine,
    repo_label: Option<String>,
) -> Result<(), GnxError> {
    let hits = compute_single(&pattern, &mode, kind_filter.as_deref(), engine, repo_label)?;
    let buckets = BucketedResults::partition(hits);
    emit_bucketed(&buckets, format, None)
}

/// Pure compute path for single-repo search: returns owned Hit rows, all
/// candidates (bucketing + per-bucket TOP_K applied at emit time).
fn compute_single(
    pattern: &str,
    mode: &FindMode,
    kind_filter: Option<&str>,
    engine: &Engine,
    repo_label: Option<String>,
) -> Result<Vec<Hit>, GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let index_dir = engine.index_dir();

    let kind_set: Option<Vec<String>> =
        kind_filter.map(|s| s.split(',').map(|k| k.trim().to_lowercase()).collect());

    let _ = mode;
    let hits = bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir);
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
    let scored = match crate::search::TantivyEngine::search(index_dir, pattern, MULTI_CAP) {
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
    // Substring fallback scans every node — on large monorepos a 3-char
    // pattern can return thousands. Cap to MULTI_CAP so partition's sort
    // stays bounded on the pre_tool_use::handle hot path.
    if hits.len() > MULTI_CAP {
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(MULTI_CAP);
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
    let file_entry = &graph.files[node.file_idx.to_native() as usize];
    let file = file_entry.path.resolve(&graph.string_pool).to_string();
    let language = Language::from_path(&file).as_str().to_string();
    let category = FileCategory::from(&file_entry.category);
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
        language,
        line,
        name: name.to_string(),
        signature,
        caller_count,
        callers,
        callees,
        category,
    })
}

// ── Multi-repo fan-out ────────────────────────────────────────────────────────

fn run_multi(
    pattern: String,
    mode: FindMode,
    kind_filter: Option<String>,
    format: OutputFormat,
    targets: Vec<(String, String)>, // (repo_name, graph_path_str)
) -> Result<(), GnxError> {
    let (hits, summary) = compute_multi(&pattern, &mode, kind_filter.as_deref(), targets)?;
    let buckets = BucketedResults::partition(hits);
    emit_bucketed(&buckets, format, Some(summary))
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
    mode: &FindMode,
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

    // Collect enough candidates to fill all 5 buckets × TOP_K each.
    // Cap at TOP_K * 5 globally so the per-bucket partitioning step has
    // top-scoring representatives from every category.
    const MULTI_CAP: usize = TOP_K * 5;
    let mut heap: BinaryHeap<Reverse<OrderedHit>> = BinaryHeap::with_capacity(MULTI_CAP + 1);
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
                    if heap.len() > MULTI_CAP {
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
            language: o.language,
            line: o.line,
            name: o.name,
            signature: o.signature,
            caller_count: o.caller_count,
            callers: o.callers,
            callees: o.callees,
            category: o.category,
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
    mode: &FindMode,
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

/// Per-repo BM25 entry point for `gnx group search` and `gnx group find`.
/// Loads the engine internally from a pre-resolved graph path.
/// Returns raw `Hit` rows without emitting anything.
pub fn run_for_repo(
    engine: &Engine,
    member: &str,
    pattern: &str,
    kind: Option<&str>,
) -> Result<Vec<Hit>, GnxError> {
    compute_single(pattern, &FindMode::Bm25, kind, engine, Some(member.to_string()))
}

/// In-process BM25 entry point for hooks and other internal consumers.
/// Returns owned `Hit` rows without going through stdout / OutputFormat.
/// BM25-only — `mode` is honoured at the CLI surface (`run`) but the
/// flat `FindMatch` shape used by Exact / Fuzzy is structurally
/// different, so callers wanting those modes should use `run` and parse
/// the JSON payload. Batch mode is not exposed here — hooks always run
/// one pattern at a time.
pub fn compute_hits(args: FindArgs, engine: &Engine) -> Result<Vec<Hit>, GnxError> {
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

/// Resolve `--repo` to `Vec<(display_name, graph_path_str)>`.
/// Returns empty Vec when the selector is absent (caller uses pre-loaded engine).
fn resolve_targets(selector: Option<&str>) -> Result<Vec<(String, String)>, GnxError> {
    use crate::commit_lookup::CommitIndex;

    let sel = match selector {
        None | Some(".") | Some("") => return Ok(vec![]),
        Some(s) => s,
    };

    let home_gnx = resolve_home_gnx();
    let registry = Registry::open(&home_gnx)
        .map_err(|e| GnxError::InvalidArgument(format!("open registry: {e}")))?;
    let snapshot = registry.snapshot();

    // Expand selector into dir_names (v2 key).
    let dir_names: Vec<String> = if sel == "@all" {
        snapshot.repos.keys().cloned().collect()
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
        // Comma-separated list of names or dir_names.
        sel.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .flat_map(|name| {
                // Match by alias or dir_name; return the dir_name (map key).
                snapshot
                    .repos
                    .iter()
                    .find(|(_k, v)| v.dir_name == name || v.aliases.iter().any(|a| a == &name))
                    .map(|(k, _v)| k.clone())
            })
            .collect()
    };

    if dir_names.is_empty() {
        return Ok(vec![]);
    }

    let mut targets: Vec<(String, String)> = Vec::with_capacity(dir_names.len());
    for dir_name in &dir_names {
        let alias = match snapshot.repos.get(dir_name) {
            Some(a) => a,
            None => continue,
        };
        let commits_dir = home_gnx.join(dir_name).join("commits");
        let idx = CommitIndex::scan(&commits_dir)
            .map_err(|e| GnxError::InvalidArgument(format!("{dir_name}: scan commits: {e}")))?;
        if idx.is_empty() {
            continue; // repo registered but not yet built
        }
        let Some(graph_path) = crate::commit_lookup::find_latest_by_mtime(&commits_dir)
            .map(|d| d.join("graph.bin"))
        else {
            continue;
        };
        let display_name = alias.aliases.first().cloned().unwrap_or_else(|| dir_name.clone());
        targets.push((display_name, graph_path.to_string_lossy().into_owned()));
    }

    Ok(targets)
}

// ── Emission ──────────────────────────────────────────────────────────────────

fn hit_to_json(h: &Hit) -> serde_json::Value {
    serde_json::json!({
        "repo": h.repo,
        "name": h.name,
        "kind": h.kind,
        "file": h.file,
        "language": h.language,
        "line": h.line,
        "signature": h.signature,
        "caller_count": h.caller_count,
        "score": h.score,
        "score_source": h.score_source.as_str(),
    })
}

fn hit_to_text(h: &Hit) -> String {
    let repo_prefix = h
        .repo
        .as_deref()
        .map(|r| format!("@{r} "))
        .unwrap_or_default();
    format!(
        "[{}] {}{}:{} ({}) {} callers:{} [score:{:.4} source:{}]",
        h.kind,
        repo_prefix,
        h.file,
        h.line,
        h.name,
        h.language,
        h.caller_count,
        h.score,
        h.score_source.as_str(),
    )
}

fn emit_bucketed(
    buckets: &BucketedResults,
    format: OutputFormat,
    summary: Option<String>,
) -> Result<(), GnxError> {
    let all_empty = buckets.source.is_empty()
        && buckets.tests.is_empty()
        && buckets.reference.is_empty()
        && buckets.document.is_empty()
        && buckets.config.is_empty();

    if all_empty {
        let hint =
            "No matches found. Try a shorter pattern or `gnx find --mode fuzzy <fragment>`.";
        match format {
            OutputFormat::Text => {
                return emit(
                    &serde_json::json!({ "results": [serde_json::Value::String(hint.into())] }),
                    format,
                );
            }
            _ => {
                return emit(
                    &serde_json::json!({
                        "status": "success",
                        "source": [],
                        "tests": [],
                        "reference": [],
                        "document": [],
                        "config": [],
                        "hint": hint,
                    }),
                    format,
                );
            }
        }
    }

    match format {
        OutputFormat::Text => {
            let mut lines: Vec<serde_json::Value> = Vec::new();
            if let Some(s) = &summary {
                lines.push(serde_json::Value::String(s.clone()));
            }
            for (label, bucket) in [
                ("source", &buckets.source),
                ("tests", &buckets.tests),
                ("reference", &buckets.reference),
                ("document", &buckets.document),
                ("config", &buckets.config),
            ] {
                lines.push(serde_json::Value::String(format!("=== {label} ===")));
                if bucket.is_empty() {
                    lines.push(serde_json::Value::String("(none)".into()));
                } else {
                    for h in bucket.iter() {
                        lines.push(serde_json::Value::String(hit_to_text(h)));
                    }
                }
            }
            emit(&serde_json::json!({ "results": lines }), format)
        }
        OutputFormat::Json | OutputFormat::Toon | OutputFormat::Llm => {
            let bucket_json = |bucket: &[Hit]| -> serde_json::Value {
                serde_json::Value::Array(bucket.iter().map(hit_to_json).collect())
            };
            let mut payload = serde_json::json!({
                "status": "success",
                "source": bucket_json(&buckets.source),
                "tests": bucket_json(&buckets.tests),
                "reference": bucket_json(&buckets.reference),
                "document": bucket_json(&buckets.document),
                "config": bucket_json(&buckets.config),
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
                language: "Rust".into(),
                line: i as u32,
                name: "n".into(),
                kind: "Function".into(),
                signature: "fn n".into(),
                caller_count: 0,
                callers: vec![],
                callees: vec![],
                score_source: ScoreSource::Bm25,
                category: FileCategory::Source,
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
        fn _check(_: fn(FindArgs, &Engine) -> Result<Vec<Hit>, GnxError>) {}
        _check(compute_hits);
    }
}
