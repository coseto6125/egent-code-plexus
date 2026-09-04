//! `ecp find` — unified symbol lookup. Three modes selectable via `--mode`:
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
//! first `ecp admin index` has materialised the lexical index. Every hit
//! carries a `language` field derived from file extension.

use crate::commands::format::kind_to_str;
use crate::commands::graph_csr::iter_incoming_edges_filtered;
use crate::engine::Engine;
use crate::output::{emit_with_caveat, OutputFormat};
use clap::{Args, ValueEnum};
use ecp_analyzer::resolution::index::Language;
use ecp_core::graph::{ArchivedFileCategory, ArchivedRelType, ArchivedZeroCopyGraph, FileCategory};
use ecp_core::registry::{resolve_home_ecp, CommitDirName, Registry};
use ecp_core::EcpError;
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

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

    /// Include hits from test files in `fuzzy` mode
    /// (skipped by default). `exact` mode automatically searches tests
    /// without needing this flag. `bm25` mode bucketises into a separate
    /// `tests` array and is unaffected by this flag.
    #[arg(long)]
    pub include_tests: bool,

    /// Filter by node kinds (csv: function,method,class,...).
    #[arg(long)]
    pub kind: Option<String>,

    /// Disambiguate when name has multiple matches: substring on file path.
    /// Same matching rule as `ecp impact --file`.
    #[arg(long)]
    pub file: Option<String>,

    /// Repository selector (path | name | @all | csv mix). Defaults to cwd.
    /// `@<group>` is rejected at the top level — use `ecp group find` instead.
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

pub fn run(args: FindArgs, engine: &Engine) -> Result<(), EcpError> {
    let mode = args.effective_mode();

    // --batch is BM25-only; reject it early in other modes so users see
    // the misconfiguration rather than silently falling back to a single
    // exact-mode query against an empty stdin.
    if args.batch && mode != FindMode::Bm25 {
        return Err(EcpError::InvalidArgument(
            "--batch is only supported with `--mode bm25`".into(),
        ));
    }

    // Registry selectors (`@all`, comma lists, repo names) are resolved by
    // the bm25 fan-out only; exact/fuzzy always query the single engine
    // main.rs loaded. Reject the combination instead of silently answering
    // from the cwd repo as if it covered the requested set.
    if mode != FindMode::Bm25 {
        if let Some(sel) = args.repo.as_deref() {
            if !matches!(sel, "." | "") && !std::path::Path::new(sel).is_dir() {
                return Err(EcpError::InvalidArgument(format!(
                    "--repo {sel}: registry selectors are only supported with `--mode bm25` \
                     (exact/fuzzy query one repo); pass a repo path instead"
                )));
            }
        }
    }

    match mode {
        FindMode::Exact | FindMode::Fuzzy => run_exact_or_fuzzy(args, engine, mode),
        FindMode::Bm25 => run_bm25(args, engine),
    }
}

fn run_bm25(args: FindArgs, engine: &Engine) -> Result<(), EcpError> {
    if args.batch {
        return run_batch(args, engine);
    }

    let format = OutputFormat::parse(args.format.as_deref());
    let pattern = args.pattern.clone().ok_or_else(|| {
        EcpError::InvalidArgument("pattern is required (or use --batch to read from stdin)".into())
    })?;

    let targets = resolve_targets(args.repo.as_deref())?;

    if targets.is_empty() {
        let caveat = engine.caveat();
        run_single(pattern, args.mode, args.kind, format, engine, None, caveat)
    } else if targets.len() == 1 {
        let target = targets.into_iter().next().unwrap();
        let local_engine = crate::auto_ensure::load_ensured(
            std::path::Path::new(&target.graph_path),
            std::path::Path::new(&target.worktree_root),
        )
        .map_err(|e| EcpError::Rkyv(format!("{}: {e}", target.display_name)))?;
        let caveat = single_target_caveat(&target, &local_engine);
        run_single(
            pattern,
            args.mode,
            args.kind,
            format,
            &local_engine,
            Some(target.display_name),
            caveat,
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
    /// Total exact/fuzzy candidates considered before default truncation.
    /// Surfaces silent take(1) so LLM consumers can tell `matches.len() == 1`
    /// apart from "1 of N picked".
    pub total_candidates: u32,
    /// matches.len() — equals total_candidates when `--all` is set.
    pub returned: u32,
    /// Fuzzy-mode matches dropped because they live in a Test file and
    /// `--include-tests` wasn't passed. Always 0 in Exact mode (test
    /// exclusion only applies to Fuzzy).
    pub tests_excluded: u32,
    pub status: String,
}

/// Category sort priority for exact/fuzzy ranking — lower is preferred.
/// `Reference` (vendored / third-party) gets the lowest priority because
/// these lookups are about the user's code, not their dependencies.
fn category_priority(cat: &ArchivedFileCategory) -> u8 {
    match cat {
        ArchivedFileCategory::Source => 0,
        ArchivedFileCategory::Example => 1,
        ArchivedFileCategory::Document => 2,
        ArchivedFileCategory::Config => 3,
        ArchivedFileCategory::Test => 4,
        ArchivedFileCategory::Reference => 5,
    }
}

fn category_to_str(cat: &ArchivedFileCategory) -> &'static str {
    match cat {
        ArchivedFileCategory::Source => "Source",
        ArchivedFileCategory::Example => "Example",
        ArchivedFileCategory::Test => "Test",
        ArchivedFileCategory::Document => "Document",
        ArchivedFileCategory::Config => "Config",
        ArchivedFileCategory::Reference => "Reference",
    }
}

/// Number of `Calls` edges into `node_idx`.
///
/// The raw in-degree is not that number: it also counts the `Defines` edge
/// from the declaring file and one `Imports` edge per file that pulls that
/// module in. Reported under the name `caller_count`, it told an agent that
/// `compute_hits` in this repo had 17 callers when the graph held 11 `Calls`
/// edges: the other 6 were the declaring file and 5 importing files.
fn count_incoming(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> u32 {
    iter_incoming_edges_filtered(graph, node_idx as u32, |rel| {
        matches!(rel, ArchivedRelType::Calls)
    })
    .count() as u32
}

/// Sort priority for an overlay-only hit, derived from its path alone (no base
/// file entry exists). Test paths rank with `Test`; everything else is treated
/// as `Source` — the common case for a working-tree edit. A finer category
/// split waits for T7-7 promotion (which gives overlay nodes a real file entry).
fn category_priority_for_path(rel_path: &str) -> u8 {
    if ecp_core::algorithms::process_trace::is_test_path(rel_path) {
        category_priority(&ArchivedFileCategory::Test)
    } else {
        category_priority(&ArchivedFileCategory::Source)
    }
}

/// Collect overlay-only symbols (present in the L1 session overlay but NOT in
/// the base graph) matching `pattern`/`mode`/`kind_filter`, as ready-to-rank
/// `FindMatch`es.
///
/// Gating: returns immediately when no overlay dir is attached (the clean-tree
/// common case), so the query hot path never builds the base-uid dedup set or
/// touches `graph`. The set is built only after `load_overlay_hits` yields at
/// least one matching hit.
///
/// Scope: `Method` symbols are NOT surfaced. The base graph keys a method's uid
/// on its owning class (`uid::compute(kind, path, owner_class, name)`), but
/// overlay fragments don't carry `owner_class`, so an overlay method's uid can
/// never match its base counterpart — it would always look "new" and duplicate
/// a method the base already has. Surfacing methods correctly needs `owner_class`
/// in the fragment (T7-7). Free functions / structs / etc. have `owner_class =
/// None` on both sides, so their uids match and dedup works.
fn overlay_only_matches(
    engine: &Engine,
    graph: &ArchivedZeroCopyGraph,
    pattern: &str,
    mode: FindMode,
    kind_filter: Option<&[String]>,
    file_filter: Option<&str>,
) -> Vec<FindMatch> {
    use ecp_core::graph::NodeKind;
    let Some(dir) = engine.overlay_dir() else {
        return Vec::new();
    };
    let Ok(hits) = crate::session::overlay_reader::load_overlay_hits(dir) else {
        return Vec::new();
    };
    let matched: Vec<_> = hits
        .into_iter()
        .filter(|h| !matches!(h.kind, NodeKind::Method | NodeKind::Constructor))
        .filter(|h| match mode {
            FindMode::Exact => h.name == pattern,
            FindMode::Fuzzy => h.name.contains(pattern),
            FindMode::Bm25 => false,
        })
        .filter(|h| {
            kind_filter.is_none_or(|kinds| {
                let k = crate::commands::format::node_kind_to_str(&h.kind).to_ascii_lowercase();
                kinds.iter().any(|want| want == &k)
            })
        })
        .filter(|h| file_filter.is_none_or(|needle| h.rel_path.contains(needle)))
        .collect();
    if matched.is_empty() {
        return Vec::new();
    }

    // Dedup against the base graph: a symbol the base already carries (with its
    // real edges) should not get a lower-fidelity overlay duplicate. The map
    // holds only the candidate uids, so a dirty-tree query on a 500k-node graph
    // pays one pass over the uids and not a 500k-entry set.
    let wanted: rustc_hash::FxHashSet<u64> = matched.iter().map(|h| h.uid).collect();
    let in_base = index_wanted_uids(graph.nodes.iter().map(|n| n.uid.to_native()), &wanted);

    matched
        .into_iter()
        .filter(|h| !in_base.contains_key(&h.uid))
        .map(|h| {
            let category = if ecp_core::algorithms::process_trace::is_test_path(&h.rel_path) {
                "Test"
            } else {
                "Source"
            };
            let kind_str = crate::commands::format::node_kind_to_str(&h.kind).to_string();
            let signature = format!("{kind_str} {}", h.name);
            FindMatch {
                file: h.rel_path,
                line: h.line,
                name: h.name,
                kind: kind_str,
                category: category.to_string(),
                caller_count: 0,
                signature,
            }
        })
        .collect()
}

fn run_exact_or_fuzzy(args: FindArgs, engine: &Engine, mode: FindMode) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());
    let pattern = args.pattern.as_deref().ok_or_else(|| {
        EcpError::InvalidArgument("pattern is required in exact / fuzzy mode".into())
    })?;

    let kind_filter: Option<Vec<String>> = args.kind.as_deref().map(|s| {
        s.split(',')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .collect()
    });
    let file_filter: Option<&str> = args.file.as_deref();

    // A ranked candidate. `src` is the base-graph node index, or an overlay-only
    // symbol that has no base node. Tuple fields after it: caller_count, category
    // priority, file path (the sort keys).
    enum CandSrc {
        Base(usize),
        Overlay(FindMatch),
    }

    let mut tests_excluded: u32 = 0;
    let mut candidates: Vec<(CandSrc, u32, u8, String)> = graph
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

            if !node.has_owning_file() {
                return None;
            }
            let file = &graph.files[node.file_idx.to_native() as usize];
            let file_path = file.path.resolve(&graph.string_pool).to_string();

            if let Some(needle) = file_filter {
                if !file_path.contains(needle) {
                    return None;
                }
            }

            let is_exact = matches!(mode, FindMode::Exact);
            if !args.include_tests
                && !is_exact
                && matches!(file.category, ArchivedFileCategory::Test)
            {
                tests_excluded += 1;
                return None;
            }

            let prio = category_priority(&file.category);
            let caller_count = count_incoming(graph, node_idx);
            Some((CandSrc::Base(node_idx), caller_count, prio, file_path))
        })
        .collect();

    // Inject symbols that live ONLY in the L1 session overlay (a working-tree
    // edit the L2 graph hasn't absorbed) so `find` reflects uncommitted changes.
    // Costs nothing on a clean tree: `overlay_only_matches` short-circuits before
    // touching the graph when no overlay dir is attached. Overlay nodes have no
    // base file entry / edges, so caller_count is 0 and category is derived from
    // the path; full edge/impact integration is the T7-7 promotion concern.
    for m in overlay_only_matches(
        engine,
        graph,
        pattern,
        mode,
        kind_filter.as_deref(),
        file_filter,
    ) {
        let prio = category_priority_for_path(&m.file);
        candidates.push((CandSrc::Overlay(m), 0, prio, String::new()));
    }

    // Sort: category priority asc, caller_count desc, file path asc.
    candidates.sort_unstable_by(|a, b| {
        a.2.cmp(&b.2)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.3.cmp(&b.3))
    });

    let total_candidates = candidates.len() as u32;
    let selected: Vec<_> = if args.all {
        candidates
    } else {
        candidates.into_iter().take(1).collect()
    };

    let matches: Vec<FindMatch> = selected
        .into_iter()
        .map(|(src, caller_count, _, _)| match src {
            CandSrc::Overlay(m) => m,
            CandSrc::Base(node_idx) => {
                let node = &graph.nodes[node_idx];
                let file = &graph.files[node.file_idx.to_native() as usize];
                let kind_str = kind_to_str(&node.kind).to_string();
                let signature = format!("{kind_str} {}", node.name.resolve(&graph.string_pool));
                FindMatch {
                    file: file.path.resolve(&graph.string_pool).to_string(),
                    line: node.start_line(),
                    name: node.name.resolve(&graph.string_pool).to_string(),
                    kind: kind_str,
                    category: category_to_str(&file.category).to_string(),
                    caller_count,
                    signature,
                }
            }
        })
        .collect();

    let returned = matches.len() as u32;
    let found = returned > 0;
    let omitted = total_candidates - returned;

    match format {
        OutputFormat::Text => {
            // The JSON/toon paths carry the staleness caveat in the `result`
            // field; text (the default format) must say it too, or a stale
            // "no match" reads as a definitive "does not exist".
            if let Some(c) = engine.caveat() {
                eprintln!("note: {c}");
            }
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
            if omitted > 0 {
                eprintln!(
                    "note: {omitted} more candidate(s) omitted; use --all to see all {total_candidates}"
                );
            }
            if tests_excluded > 0 {
                eprintln!(
                    "note: {tests_excluded} test-file match(es) hidden; pass --include-tests to surface them"
                );
            }
            Ok(())
        }
        _ => {
            let result = FindResult {
                found,
                matches,
                total_candidates,
                returned,
                tests_excluded,
                status: "success".to_string(),
            };
            emit_with_caveat(
                &serde_json::to_value(&result).map_err(|e| EcpError::Output(e.to_string()))?,
                format,
                engine.caveat(),
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
fn run_batch(args: FindArgs, engine: &Engine) -> Result<(), EcpError> {
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
        let target = &targets[0];
        let eng = crate::auto_ensure::load_ensured(
            std::path::Path::new(&target.graph_path),
            std::path::Path::new(&target.worktree_root),
        )
        .map_err(|e| EcpError::InvalidArgument(format!("{}: {e}", target.display_name)))?;
        Some((target.display_name.clone(), eng))
    } else {
        None
    };
    let multi_repo_engines: Option<Vec<(String, Result<Engine, String>)>> = if targets.len() > 1 {
        Some(load_engines_lossy(&targets))
    } else {
        None
    };

    // Staleness is a property of the loaded engines, not of any one query —
    // compute the caveat once and stamp it on every per-pattern emission.
    let batch_caveat: Option<String> = if let Some(loaded) = multi_repo_engines.as_ref() {
        stale_repos_caveat(&targets, loaded)
    } else if let Some((_, local_engine)) = single_repo_engine.as_ref() {
        single_target_caveat(&targets[0], local_engine)
    } else {
        engine.caveat()
    };

    for pattern in &queries {
        println!("=== pattern: {pattern} ===");

        let hits = if targets.is_empty() {
            compute_single(pattern, &args.mode, args.kind.as_deref(), engine, None)?.0
        } else if let Some((repo_name, local_engine)) = single_repo_engine.as_ref() {
            compute_single(
                pattern,
                &args.mode,
                args.kind.as_deref(),
                local_engine,
                Some(repo_name.clone()),
            )?
            .0
        } else {
            let loaded = multi_repo_engines.as_ref().unwrap();
            let (hits, _summary) =
                compute_multi_with_engines(pattern, &args.mode, args.kind.as_deref(), loaded);
            hits
        };

        let buckets = BucketedResults::partition(hits);
        emit_bucketed(&buckets, format, None, batch_caveat.clone())?;
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
    /// Real callee count from `out_offsets`. Mirrors `caller_count`; the
    /// `callees` list below is capped at `HOP_EXPANSION_LIMIT`, so an LLM
    /// must read this to know how many callees actually exist.
    pub callee_count: usize,
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
    callee_count: usize,
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
            callee_count: h.callee_count,
            callees: h.callees,
            score_source: h.score_source,
            category: h.category,
        }
    }
}

/// Six-bucket output — one per `FileCategory`. Empty buckets emit `[]` in
/// JSON and `(none)` in text; each bucket independently capped at `TOP_K`.
/// `Example` (framework demo / sample / `examples/` apps) buckets separately
/// from `tests` because canonical framework references and test fixtures
/// answer different LLM questions ("how do I use X" vs "how is X tested").
struct BucketedResults {
    source: Vec<Hit>,
    examples: Vec<Hit>,
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
        let mut examples = Vec::new();
        let mut tests = Vec::new();
        let mut reference = Vec::new();
        let mut document = Vec::new();
        let mut config = Vec::new();

        for h in hits {
            let bucket = match h.category {
                FileCategory::Source => &mut source,
                FileCategory::Example => &mut examples,
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
            examples,
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
    caveat: Option<String>,
) -> Result<(), EcpError> {
    let (hits, truncated_total) =
        compute_single(&pattern, &mode, kind_filter.as_deref(), engine, repo_label)?;
    let buckets = BucketedResults::partition(hits);
    let summary = if truncated_total > (MULTI_CAP as u64) {
        eprintln!("note: search truncated — {truncated_total} matches found, {MULTI_CAP} kept");
        Some(format!(
            "search truncated: {truncated_total} matches found, {MULTI_CAP} kept"
        ))
    } else {
        None
    };
    emit_bucketed_with_metadata(&buckets, format, summary, truncated_total, caveat)
}

/// Pure compute path for single-repo search: returns owned Hit rows, all
/// candidates (bucketing + per-bucket TOP_K applied at emit time). The
/// `u64` carries the pre-truncate total when the substring fallback
/// dropped rows; equals `hits.len()` when nothing was capped.
fn compute_single(
    pattern: &str,
    mode: &FindMode,
    kind_filter: Option<&str>,
    engine: &Engine,
    repo_label: Option<String>,
) -> Result<(Vec<Hit>, u64), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let index_dir = engine.index_dir();

    let kind_set: Option<Vec<String>> =
        kind_filter.map(|s| s.split(',').map(|k| k.trim().to_lowercase()).collect());

    let _ = mode;
    Ok(bm25_hits_from_graph(
        graph,
        pattern,
        &kind_set,
        &repo_label,
        index_dir,
    ))
}

/// Primary BM25 path: queries the persisted Tantivy index when present,
/// falling back to a per-name substring scan (exact 1.0 / prefix 0.7 /
/// substring 0.4) when `<index_dir>/tantivy/` is missing — which happens
/// on a freshly-cloned repo before `ecp admin index` has run.
fn bm25_hits_from_graph(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: Option<&std::path::Path>,
) -> (Vec<Hit>, u64) {
    if let Some(dir) = index_dir {
        if dir.join("tantivy").exists() {
            return tantivy_hits(graph, pattern, kind_set, repo_label, dir);
        }
    }
    substring_hits(graph, pattern, kind_set, repo_label)
}

/// Build a `uid -> node_idx` map restricted to `wanted`, in a single pass over
/// the node uids (yielded in graph order). Only the matched uids are inserted,
/// so a query touching <=MULTI_CAP results skips the ~N inserts (and backing
/// resize) a full-graph map would cost on a 500k-node graph. On a uid collision
/// the last index in scan order wins, matching a naive full-insert pass.
///
/// Distinct from `ecp_core::graph_query::build_uid_index`, which materialises
/// the *whole* graph's uid table for callers (e.g. BFS) that resolve arbitrary
/// uids; here the lookup set is bounded by `scored`, so a subset map is cheaper.
fn index_wanted_uids(
    node_uids: impl Iterator<Item = u64>,
    wanted: &rustc_hash::FxHashSet<u64>,
) -> rustc_hash::FxHashMap<u64, usize> {
    let mut uid_to_idx: rustc_hash::FxHashMap<u64, usize> =
        rustc_hash::FxHashMap::with_capacity_and_hasher(wanted.len(), Default::default());
    for (idx, uid) in node_uids.enumerate() {
        if wanted.contains(&uid) {
            uid_to_idx.insert(uid, idx);
        }
    }
    uid_to_idx
}

/// Query the on-disk Tantivy BM25 index, map uids back to graph nodes,
/// and materialise `Hit` rows. Returns an empty vec when the index opens
/// but yields no matches; falls through to substring scan if the query
/// fails outright (e.g. corrupt segment), preserving the contract that
/// hooks never error out on search.
fn tantivy_hits(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: &std::path::Path,
) -> (Vec<Hit>, u64) {
    let (scored, tantivy_total) =
        match crate::search::TantivyEngine::search(index_dir, pattern, MULTI_CAP) {
            Some(s) => s,
            // Index unavailable / corrupt / parse error — fall through so
            // hook context isn't silently empty.
            None => return substring_hits(graph, pattern, kind_set, repo_label),
        };
    // Index ran cleanly. An empty scored vec means BM25 ruled out every
    // symbol; we MUST NOT fall back to substring scan, since that would
    // surface 0.4-scored noise the trusted index already rejected.
    if scored.is_empty() {
        return (Vec::new(), 0);
    }

    // Tantivy stores uid as the decimal string of the u64 hash; parse back for
    // O(1) lookup. `scored` is capped at MULTI_CAP, so we only need a map of
    // those entries rather than the whole graph (see `index_wanted_uids`).
    let wanted: rustc_hash::FxHashSet<u64> = scored
        .iter()
        .filter_map(|(_, uid)| uid.parse::<u64>().ok())
        .collect();
    let uid_to_idx = index_wanted_uids(graph.nodes.iter().map(|n| n.uid.to_native()), &wanted);

    let mut hits = Vec::with_capacity(scored.len());
    for (score, uid) in scored {
        let Ok(uid_u64) = uid.parse::<u64>() else {
            continue;
        };
        let Some(&idx) = uid_to_idx.get(&uid_u64) else {
            continue;
        };
        if let Some(hit) = build_hit(graph, idx, score, ScoreSource::Bm25, kind_set, repo_label) {
            hits.push(hit);
        }
    }
    (hits, tantivy_total)
}

/// Fallback BM25-shaped scan when no tantivy index is on disk.
/// Preserves the legacy 1.0 / 0.7 / 0.4 scoring so hook output stays
/// shaped the same before the first `ecp admin index` has produced an
/// index.
fn substring_hits(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
) -> (Vec<Hit>, u64) {
    let pattern_lower = pattern.to_lowercase();
    // A symbol name never contains whitespace, so a multi-word pattern (the
    // hook's `pub struct HookInput`) is scored one term at a time and a name
    // takes its best term; the whole string would match nothing.
    let terms: Vec<&str> = if pattern_lower.trim().is_empty() {
        vec![""]
    } else {
        pattern_lower.split_whitespace().collect()
    };
    let mut scored: Vec<(f32, usize)> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(idx, _)| hit_eligible(graph, *idx, kind_set))
        .filter_map(|(idx, node)| {
            let name = node.name.resolve(&graph.string_pool);
            terms
                .iter()
                .filter_map(|term| substring_score(name, term))
                .reduce(f32::max)
                .map(|score| (score, idx))
        })
        .collect();
    // Best score first, always. Consumers that take the rows as they come
    // used to get node order, which hid an exact match behind earlier 0.4
    // substring rows: the PreToolUse hook (leading MAX_HITS rows),
    // `ecp group find --merge none` (prints per-repo order) and `--merge rrf`
    // (ranks by position), and the tie order of a multi-repo `find`. Those
    // change on purpose. `run_single` / `run_batch` re-sort stably in the
    // bucket partition, so their output is unchanged. Substring fallback
    // scans every node; a 3-char pattern on a monorepo returns thousands, so
    // cap to MULTI_CAP before the rows are built: a `Hit` carries its
    // callers and callees as owned Strings, and building thousands to keep
    // a hundred was the peak allocation of this path.
    let total_before_truncate = scored.len() as u64;
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(MULTI_CAP);
    let hits = scored
        .into_iter()
        .filter_map(|(score, idx)| {
            build_hit(
                graph,
                idx,
                score,
                ScoreSource::Substring,
                kind_set,
                repo_label,
            )
        })
        .collect();
    (hits, total_before_truncate)
}

/// Legacy 1.0 / 0.7 / 0.4 substring score. ASCII names compare in place;
/// the per-node `to_lowercase` allocation this replaces ran once for every
/// node in the graph, matches or not. `pattern_lower` is already lowercased.
fn substring_score(name: &str, pattern_lower: &str) -> Option<f32> {
    // The CLI accepts `ecp find ""`: every name starts with the empty
    // pattern (0.7, or 1.0 for an empty name), and `windows(0)` below would
    // panic, so settle it here rather than by branch order.
    if pattern_lower.is_empty() {
        return Some(if name.is_empty() { 1.0 } else { 0.7 });
    }
    if !name.is_ascii() || !pattern_lower.is_ascii() {
        let name_lower = name.to_lowercase();
        return if name_lower == pattern_lower {
            Some(1.0)
        } else if name_lower.starts_with(pattern_lower) {
            Some(0.7)
        } else if name_lower.contains(pattern_lower) {
            Some(0.4)
        } else {
            None
        };
    }
    let (name, pattern) = (name.as_bytes(), pattern_lower.as_bytes());
    if name.eq_ignore_ascii_case(pattern) {
        Some(1.0)
    } else if name.len() >= pattern.len() && name[..pattern.len()].eq_ignore_ascii_case(pattern) {
        Some(0.7)
    } else if name
        .windows(pattern.len())
        .any(|window| window.eq_ignore_ascii_case(pattern))
    {
        Some(0.4)
    } else {
        None
    }
}

/// The `--kind` filter plus the owning-file requirement, split out so a
/// caller can rank and cap candidates before paying for `build_hit`.
fn hit_eligible(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    idx: usize,
    kind_set: &Option<Vec<String>>,
) -> bool {
    let node = &graph.nodes[idx];
    if let Some(ks) = kind_set {
        let node_kind_str = format!("{:?}", node.kind).to_lowercase();
        if !ks.iter().any(|k| k == &node_kind_str) {
            return false;
        }
    }
    node.has_owning_file()
}

/// Shared per-node Hit constructor. Applies kind filter and reads
/// file/line/kind/caller_count from the archived graph. Returns `None`
/// when the node's kind doesn't match the filter. `score_source`
/// annotates which ranker produced `score` (BM25 / substring).
fn build_hit(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    idx: usize,
    score: f32,
    score_source: ScoreSource,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
) -> Option<Hit> {
    if !hit_eligible(graph, idx, kind_set) {
        return None;
    }
    let node = &graph.nodes[idx];
    let name = node.name.resolve(&graph.string_pool);
    let file_entry = &graph.files[node.file_idx.to_native() as usize];
    let file = file_entry.path.resolve(&graph.string_pool).to_string();
    let language = Language::from_path(&file).as_str().to_string();
    let category = FileCategory::from(&file_entry.category);
    let line = node.start_line();
    let kind_str = kind_to_str(&node.kind).to_string();
    let signature = format!("{kind_str} {name}");

    // `Calls` only, on both sides. Walking the raw CSR slice put the declaring
    // file and every importing file into `callers`, and every `ReadsField`
    // target into `callees` — the hook renders those two lists verbatim as
    // `Called by:` and `Calls:`, so a File node arrived at the model labelled
    // as a caller.
    let caller_names = iter_incoming_edges_filtered(graph, idx as u32, |rel| {
        matches!(rel, ArchivedRelType::Calls)
    })
    .map(|(src, _)| {
        graph.nodes[src as usize]
            .name
            .resolve(&graph.string_pool)
            .to_string()
    });
    let mut caller_count = 0usize;
    let mut callers: Vec<String> = Vec::new();
    for name in caller_names {
        caller_count += 1;
        if callers.len() < HOP_EXPANSION_LIMIT {
            callers.push(name);
        }
    }

    let out_start = graph.out_offsets[idx].to_native() as usize;
    let out_end = graph.out_offsets[idx + 1].to_native() as usize;
    let mut callee_count = 0usize;
    let mut callees: Vec<String> = Vec::new();
    for e in graph.edges[out_start..out_end]
        .iter()
        .filter(|e| matches!(e.rel_type, ArchivedRelType::Calls))
    {
        callee_count += 1;
        if callees.len() < HOP_EXPANSION_LIMIT {
            callees.push(
                graph.nodes[e.target.to_native() as usize]
                    .name
                    .resolve(&graph.string_pool)
                    .to_string(),
            );
        }
    }

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
        callee_count,
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
    targets: Vec<RepoTarget>, // (repo_name, graph_path_str, worktree_root)
) -> Result<(), EcpError> {
    let loaded = load_engines_lossy(&targets);
    let caveat = stale_repos_caveat(&targets, &loaded);
    let (hits, summary) =
        compute_multi_with_engines(&pattern, &mode, kind_filter.as_deref(), &loaded);
    let buckets = BucketedResults::partition(hits);
    emit_bucketed(&buckets, format, Some(summary), caveat)
}

/// Caveat for a single resolved target: HEAD-mismatch staleness names the
/// repo; otherwise the engine's own warm-attach caveat (if any) applies.
fn single_target_caveat(target: &RepoTarget, engine: &Engine) -> Option<String> {
    if target.stale_for_head {
        stale_graph_caveat(&[target.display_name.as_str()])
    } else {
        engine.caveat()
    }
}

/// Pre-load engines for a batch of target repos. Each engine load is
/// captured as a per-repo `Result<Engine, String>` so individual
/// failures don't kill the whole multi-repo query — the failing repo
/// contributes 0 hits and is counted in the summary.
pub(crate) fn load_engines_lossy(targets: &[RepoTarget]) -> Vec<(String, Result<Engine, String>)> {
    targets
        .iter()
        .map(|target| {
            let result = crate::auto_ensure::load_ensured(
                std::path::Path::new(&target.graph_path),
                std::path::Path::new(&target.worktree_root),
            );
            (target.display_name.clone(), result)
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
                        // Multi-repo path: cross-repo top-K merging handles its
                        // own truncation, so the substring-fallback truncate
                        // signal from per-repo bm25 is discarded here.
                        let (hits, _truncated_total) = bm25_hits_from_graph(
                            graph,
                            pattern,
                            &kind_set,
                            &repo_label,
                            engine.index_dir(),
                        );
                        hits
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
    ordered.sort_by_key(|b| std::cmp::Reverse(b.score_bits));

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
            callee_count: o.callee_count,
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

/// Per-repo BM25 entry point for `ecp group search` and `ecp group find`.
/// Loads the engine internally from a pre-resolved graph path.
/// Returns raw `Hit` rows without emitting anything.
pub fn run_for_repo(
    engine: &Engine,
    member: &str,
    pattern: &str,
    kind: Option<&str>,
) -> Result<Vec<Hit>, EcpError> {
    compute_single(
        pattern,
        &FindMode::Bm25,
        kind,
        engine,
        Some(member.to_string()),
    )
    .map(|(hits, _truncated)| hits)
}

/// In-process BM25 entry point for hooks and other internal consumers.
/// Returns owned `Hit` rows without going through stdout / OutputFormat.
/// BM25-only — `mode` is honoured at the CLI surface (`run`) but the
/// flat `FindMatch` shape used by Exact / Fuzzy is structurally
/// different, so callers wanting those modes should use `run` and parse
/// the JSON payload. Batch mode is not exposed here — hooks always run
/// one pattern at a time.
pub fn compute_hits(args: FindArgs, engine: &Engine) -> Result<Vec<Hit>, EcpError> {
    let pattern = args.pattern.as_deref().ok_or_else(|| {
        EcpError::InvalidArgument("compute_hits requires a pattern (--batch not supported)".into())
    })?;
    let targets = resolve_targets(args.repo.as_deref())?;
    if targets.is_empty() {
        compute_single(pattern, &args.mode, args.kind.as_deref(), engine, None)
            .map(|(hits, _truncated)| hits)
    } else if targets.len() == 1 {
        let target = targets.into_iter().next().unwrap();
        let local_engine = crate::auto_ensure::load_ensured(
            std::path::Path::new(&target.graph_path),
            std::path::Path::new(&target.worktree_root),
        )
        .map_err(|e| EcpError::Rkyv(format!("{}: {e}", target.display_name)))?;
        compute_single(
            pattern,
            &args.mode,
            args.kind.as_deref(),
            &local_engine,
            Some(target.display_name),
        )
        .map(|(hits, _truncated)| hits)
    } else {
        let loaded = load_engines_lossy(&targets);
        let (hits, _summary) =
            compute_multi_with_engines(pattern, &args.mode, args.kind.as_deref(), &loaded);
        Ok(hits)
    }
}

// ── Repo selector resolution ─────────────────────────────────────────────────

/// One `--repo`-resolved target. `graph_path` and `worktree_root` are both
/// `String`; a struct (not a tuple) keeps them from being transposed at a
/// load site, which would silently load the wrong graph. `worktree_root` is
/// passed to `ensure_fresh` so the per-repo load gets the same version
/// (ecp-fingerprint → full rebuild) + freshness (git → incremental) checks
/// the cwd graph gets in main.rs.
pub(crate) struct RepoTarget {
    display_name: String,
    graph_path: String,
    worktree_root: String,
    /// The picked commit dir (latest published, by mtime) is behind the
    /// target worktree's HEAD. Queries read L2 only, so commits made after
    /// the last index are invisible for this repo — surfaced as a `result`
    /// caveat. The warm-attach flag can't represent this: the old graph
    /// EXISTS, so `ensure_fresh` takes the Stale → L1-refresh path and
    /// reports Ready.
    stale_for_head: bool,
}

/// Registry entries whose `dir_name` shares a substring with something the
/// user asked for, at most five, rendered as a ` Did you mean: a, b?` clause.
/// Empty when nothing is close.
///
/// A registry can hold a hundred repos. Listing them all to explain one typo
/// spends more of the reading model's context than the answer would have, so
/// the error carries the near misses and a count instead of the roster.
fn did_you_mean(unmatched: &[String], snapshot: &ecp_core::registry::RegistryFile) -> String {
    let mut near: Vec<&str> = snapshot
        .repos
        .values()
        .map(|v| v.dir_name.as_str())
        .filter(|known| {
            unmatched
                .iter()
                .any(|want| known.contains(want.as_str()) || want.contains(known))
        })
        .collect();
    if near.is_empty() {
        return String::new();
    }
    near.sort_unstable();
    near.truncate(5);
    format!(" Did you mean: {}?", near.join(", "))
}

/// Resolve `--repo` to `Vec<RepoTarget>`.
/// Returns empty Vec when the selector is absent (caller uses pre-loaded engine).
fn resolve_targets(selector: Option<&str>) -> Result<Vec<RepoTarget>, EcpError> {
    use crate::commit_lookup::CommitIndex;

    let sel = match selector {
        None | Some(".") | Some("") => return Ok(vec![]),
        // A real directory is not a registry selector. `Commands::repo()` has
        // already handed it to the engine as this invocation's repo, so the
        // empty target list correctly means "search the graph already loaded".
        // Path semantics win over an identically-named registry entry, which is
        // the trade-off `Commands::repo()` documents.
        Some(s) if std::path::Path::new(s).is_dir() => return Ok(vec![]),
        Some(s) => s,
    };

    let home_ecp = resolve_home_ecp();
    let registry = Registry::open(&home_ecp)
        .map_err(|e| EcpError::InvalidArgument(format!("open registry: {e}")))?;
    let snapshot = registry.snapshot();

    // Expand selector into dir_names (v2 key).
    let dir_names: Vec<String> = if sel == "@all" {
        snapshot.repos.keys().cloned().collect()
    } else if let Some(group_name) = sel.strip_prefix('@') {
        return Err(EcpError::InvalidArgument(format!(
            "`@{group_name}` cannot be used at the top level — use `ecp group find` instead"
        )));
    } else {
        // Comma-separated list of names or dir_names.
        let mut matched = Vec::new();
        let mut unmatched = Vec::new();
        for name in sel.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            // Match by alias or dir_name; keep the dir_name (map key).
            match snapshot
                .repos
                .iter()
                .find(|(_k, v)| v.dir_name == name || v.aliases.iter().any(|a| a == name))
            {
                Some((k, _v)) => matched.push(k.clone()),
                None => unmatched.push(name.to_string()),
            }
        }
        // A name the registry does not hold used to be dropped here, leaving
        // an empty target list that the caller reads as "no selector given" —
        // so the search silently ran against the current directory's graph and
        // answered about the wrong repository. In a mixed list the same drop
        // narrowed the search without saying so.
        if !unmatched.is_empty() {
            return Err(EcpError::InvalidArgument(format!(
                "--repo: not in the registry: {}.{} {} registered in total.",
                unmatched.join(", "),
                did_you_mean(&unmatched, &snapshot),
                snapshot.repos.len()
            )));
        }
        matched
    };

    if dir_names.is_empty() {
        return Err(EcpError::InvalidArgument(format!(
            "--repo {sel}: the registry holds no repositories to search"
        )));
    }

    let mut targets: Vec<RepoTarget> = Vec::with_capacity(dir_names.len());
    for dir_name in &dir_names {
        let alias = match snapshot.repos.get(dir_name) {
            Some(a) => a,
            None => continue,
        };
        let commits_dir = home_ecp.join(dir_name).join("commits");
        let idx = CommitIndex::scan(&commits_dir)
            .map_err(|e| EcpError::InvalidArgument(format!("{dir_name}: scan commits: {e}")))?;
        if idx.is_empty() {
            continue; // repo registered but not yet built
        }
        let Some(graph_path) =
            crate::commit_lookup::find_latest_by_mtime(&commits_dir).map(|d| d.join("graph.bin"))
        else {
            continue;
        };
        let display_name = alias
            .aliases
            .first()
            .cloned()
            .unwrap_or_else(|| dir_name.clone());
        // worktree_root for ensure_fresh = the repo's source tree, i.e. the
        // parent of its `<worktree>/.git` common_dir. The fingerprint (ecp-
        // version) check ignores it; the incremental git-status check uses it.
        let worktree_root = crate::git_cache::worktree_root_from_common_dir(std::path::Path::new(
            &alias.common_dir,
        ))
        .to_string_lossy()
        .into_owned();
        // Commit dirs are named `<prefix>__<sha>[.gen.<…>]` — a same-SHA
        // rebuild publishes a `.gen.` dir, so suffix matching would flag
        // perfectly fresh repos. Parse the SHA out and compare against HEAD;
        // an unparseable dir name proves nothing, so it stays un-flagged
        // (the engine's own warm-attach caveat still covers that load).
        let stale_for_head = crate::git_cache::head_sha(std::path::Path::new(&worktree_root))
            .zip(graph_path.parent().and_then(|d| d.file_name()))
            .map(|(head, dir)| {
                CommitDirName::parse(&dir.to_string_lossy())
                    .map(|parsed| parsed.sha_hex() != head)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        targets.push(RepoTarget {
            display_name,
            graph_path: graph_path.to_string_lossy().into_owned(),
            worktree_root,
            stale_for_head,
        });
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
        "callee_count": h.callee_count,
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
    caveat: Option<String>,
) -> Result<(), EcpError> {
    emit_bucketed_with_metadata(buckets, format, summary, 0, caveat)
}

/// Completeness caveat naming exactly WHICH repos answered from a graph
/// behind their worktree's HEAD, so fresh repos' rows keep their trust and a
/// stale repo's "no hits" can't read as definitive.
fn stale_graph_caveat(names: &[&str]) -> Option<String> {
    (!names.is_empty()).then(|| {
        format!(
            "results may be incomplete for repo(s) {}: graph predates the current HEAD there; \
             symbols added since are invisible. Rerun, or `ecp admin index --force` in the \
             stale repo for a definitive answer.",
            names.join(", ")
        )
    })
}

/// Cross-repo staleness sweep: a repo is stale when its picked commit dir is
/// behind HEAD (`stale_for_head`) or its engine warm-attached a sibling SHA.
fn stale_repos_caveat(
    targets: &[RepoTarget],
    loaded: &[(String, Result<Engine, String>)],
) -> Option<String> {
    let stale: Vec<&str> = targets
        .iter()
        .zip(loaded)
        .filter(|(target, (_, result))| {
            target.stale_for_head || result.as_ref().is_ok_and(|engine| engine.is_stale_for_sha)
        })
        .map(|(target, _)| target.display_name.as_str())
        .collect();
    stale_graph_caveat(&stale)
}

/// Same as `emit_bucketed` but threads the substring-fallback pre-truncate
/// total through the JSON payload so LLM consumers can detect that the
/// BM25 substring fallback dropped rows. `bm25_pre_truncate_total = 0`
/// means no truncation; `> MULTI_CAP` means the difference (`total - MULTI_CAP`)
/// hits were silently dropped from the bucket pool.
fn emit_bucketed_with_metadata(
    buckets: &BucketedResults,
    format: OutputFormat,
    summary: Option<String>,
    bm25_pre_truncate_total: u64,
    caveat: Option<String>,
) -> Result<(), EcpError> {
    let all_empty = buckets.source.is_empty()
        && buckets.examples.is_empty()
        && buckets.tests.is_empty()
        && buckets.reference.is_empty()
        && buckets.document.is_empty()
        && buckets.config.is_empty();

    if all_empty {
        let hint = "No matches found. Try a shorter pattern or `ecp find --mode fuzzy <fragment>`.";
        match format {
            OutputFormat::Text => {
                return emit_with_caveat(
                    &serde_json::json!({ "results": [serde_json::Value::String(hint.into())] }),
                    format,
                    caveat,
                );
            }
            _ => {
                // A stale graph's "no matches" is the most dangerous shape —
                // the caveat is what stops it reading as a definitive miss.
                return emit_with_caveat(
                    &serde_json::json!({
                        "status": "success",
                        "source": [],
                        "examples": [],
                        "tests": [],
                        "reference": [],
                        "document": [],
                        "config": [],
                        "bm25_pre_truncate_total": bm25_pre_truncate_total,
                        "hint": hint,
                    }),
                    format,
                    caveat,
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
                ("examples", &buckets.examples),
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
            emit_with_caveat(&serde_json::json!({ "results": lines }), format, caveat)
        }
        OutputFormat::Json | OutputFormat::Toon | OutputFormat::Llm => {
            let bucket_json = |bucket: &[Hit]| -> serde_json::Value {
                serde_json::Value::Array(bucket.iter().map(hit_to_json).collect())
            };
            let mut payload = serde_json::json!({
                "status": "success",
                "source": bucket_json(&buckets.source),
                "examples": bucket_json(&buckets.examples),
                "tests": bucket_json(&buckets.tests),
                "reference": bucket_json(&buckets.reference),
                "document": bucket_json(&buckets.document),
                "config": bucket_json(&buckets.config),
                "bm25_pre_truncate_total": bm25_pre_truncate_total,
            });
            if let Some(s) = summary {
                payload["summary"] = serde_json::Value::String(s);
            }
            emit_with_caveat(&payload, format, caveat)
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
                callee_count: 0,
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
        fn _check(_: fn(FindArgs, &Engine) -> Result<Vec<Hit>, EcpError>) {}
        _check(compute_hits);
    }

    // Reference: the pre-optimisation behaviour — insert every node into the
    // map. `index_wanted_uids` must agree with this for the subset it keeps,
    // including last-write-wins on a duplicate uid.
    fn full_insert(node_uids: &[u64]) -> rustc_hash::FxHashMap<u64, usize> {
        let mut m: rustc_hash::FxHashMap<u64, usize> = Default::default();
        for (idx, &uid) in node_uids.iter().enumerate() {
            m.insert(uid, idx);
        }
        m
    }

    #[test]
    fn index_wanted_uids_matches_full_insert_for_wanted_subset() {
        let nodes: [u64; 6] = [10, 20, 30, 20, 40, 10];
        let full = full_insert(&nodes);
        for wanted_uids in [
            vec![],
            vec![20u64],
            vec![10, 40],
            vec![10, 20, 30, 40, 99], // 99 absent — must be omitted, not panic
        ] {
            let wanted: rustc_hash::FxHashSet<u64> = wanted_uids.iter().copied().collect();
            let got = index_wanted_uids(nodes.iter().copied(), &wanted);
            // Same keys (intersection of wanted with present uids).
            for &uid in &wanted_uids {
                assert_eq!(got.get(&uid).copied(), full.get(&uid).copied(), "uid {uid}");
            }
            assert_eq!(
                got.len(),
                wanted_uids.iter().filter(|u| full.contains_key(u)).count()
            );
        }
    }

    #[test]
    fn index_wanted_uids_duplicate_uid_keeps_last_index() {
        // uid 7 appears at idx 0 and idx 3; both passes must keep the later 3.
        let nodes: [u64; 4] = [7, 8, 9, 7];
        let wanted: rustc_hash::FxHashSet<u64> = [7u64].into_iter().collect();
        let got = index_wanted_uids(nodes.iter().copied(), &wanted);
        assert_eq!(got.get(&7).copied(), Some(3));
        assert_eq!(full_insert(&nodes).get(&7).copied(), Some(3));
    }
}
