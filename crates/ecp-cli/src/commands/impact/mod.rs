mod baseline;
pub(crate) mod bfs;
mod coverage;
mod literal;
pub mod payload;
mod symbol;
mod unexplained;

pub use baseline::build_baseline_payload;
pub use literal::build_literal_coherence_payload;
pub use payload::{BaselinePayload, ChangedSymbol, ImpactBySymbol, SymbolImpactPayload};
pub use symbol::{run_for_symbol, LocalImpact};

use crate::engine::Engine;
use crate::output::{emit_with_caveat, merge_caveats, OutputFormat};
use crate::read_context::{resolve_target, target_request};
use clap::{Args, ValueEnum};
use ecp_core::config;
use ecp_core::contracts::identity::TargetResolution;
use ecp_core::session::MergedGraph;
use ecp_core::{EcpError, HIGH_TRUST_CONFIDENCE};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum Direction {
    #[value(alias = "upstream")]
    Up,
    #[value(alias = "downstream")]
    Down,
    Both,
}

/// Default heuristic-edge confidence gate; mirrored by all three
/// `confidence_threshold: 0.85` sites (ImpactArgs default, build_payload's
/// inner construction, review/aggregate.rs).
pub const DEFAULT_CONFIDENCE_THRESHOLD: f32 = 0.85;

/// Symbol-level blast radius. From `<name>` traverses call-graph for upstream
/// callers / downstream callees. From `--baseline <ref>`
/// detects symbols changed vs the baseline and runs the same traversal per
/// change. For edge-level resolver delta (tier degradation, silent break),
/// use `ecp diff --section bindings` instead.
#[derive(Args, Debug)]
pub struct ImpactArgs {
    /// Target symbol name (mutually exclusive with --baseline). Equivalent to
    /// the `--target` named form below. Optional when `--batch` is set or
    /// when a non-positional mode flag (`--target`, `--baseline`, `--literal`,
    /// `--literal-coherence`) supplies the query.
    #[arg(required_unless_present_any = ["target", "batch", "baseline", "literal", "literal_coherence"])]
    pub name: Option<String>,

    /// Named alias for the positional NAME argument — kept for parity with
    /// old MCP / wrapper habits.
    #[arg(long = "target", value_name = "TARGET", conflicts_with_all = ["name", "baseline", "batch"])]
    pub target: Option<String>,

    /// Git ref — compute blast radius across all symbols changed between
    /// this baseline and HEAD. Mutually exclusive with positional <name>.
    #[arg(long, conflicts_with_all = ["name", "batch"])]
    pub baseline: Option<String>,

    /// Select the declaration in this file. A file path that equals the value
    /// ranks first, then one that ends with it, then one that contains it.
    /// An ambiguous name returns candidates that you can re-run.
    /// `--file_path` / `--file-path` stay as aliases for back-compat.
    #[arg(long = "file", alias = "file_path", alias = "file-path")]
    pub file: Option<String>,

    /// Select the declaration of one node kind (function | method | class | ...).
    /// The kind filter applies before `--file` ranks the declarations.
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

    /// Test callers are listed by default, tagged `test: true`, since a rename
    /// breaks them too. A test caller sits in a test file or carries a test
    /// attribute (`#[test]`, `@Test`, `[Fact]`); a test-like name alone is
    /// none. This flag is kept for old callers; on its own
    /// it changes one thing: `--baseline` also treats changed test files as
    /// changed symbols.
    #[arg(long, aliases = ["include_tests", "includeTests"], default_value_t = false)]
    pub include_tests: bool,

    /// Drop test callers; the payload reports `hidden_test_callers: N`.
    #[arg(long, aliases = ["exclude_tests", "excludeTests"], conflicts_with_all = ["include_tests", "test_coverage"])]
    pub exclude_tests: bool,

    /// Comma-separated relation types to follow (calls, extends, ...).
    #[arg(long = "relation_types", alias = "relation-types")]
    pub relation_types: Option<String>,

    /// Repository selector.
    #[arg(long)]
    pub repo: Option<String>,

    /// Coverage gap analysis: for each touched symbol, classify by verified
    /// (untagged) test callers (uncovered / partial / covered / orphan). A
    /// test caller tagged `requires_verification` counts under
    /// `unverified_test_callers`, never as coverage. Orphan means no caller
    /// at all. Uses FunctionMeta.is_test flag from per-language extraction.
    /// Outputs uncovered symbols first to support LLM PR review ("X 改了沒測試").
    /// Needs test callers in the walk, so it conflicts with --exclude-tests.
    #[arg(long, aliases = ["test_coverage", "testCoverage"], default_value_t = false)]
    pub test_coverage: bool,

    /// Suppress heuristic callers (MirrorsField, EventTopicMirror,
    /// CompensatedBy) from the blast radius. Default: heuristic callers ARE
    /// shown, in a separate `heuristic_callers` bucket tagged
    /// `requires_verification`. With this flag, `Calls` callers below 0.7
    /// confidence still appear, tagged `requires_verification`.
    #[arg(long, default_value_t = false)]
    pub no_heuristic: bool,

    /// Informational confidence gate — promotes heuristic edges when T4-7/T5-33
    /// emit per-edge tiers. Currently controls the --explain-confidence report.
    #[arg(long, default_value_t = DEFAULT_CONFIDENCE_THRESHOLD)]
    pub confidence_threshold: f32,

    /// Emit explain_confidence block with threshold + per-tier filtered counts.
    #[arg(long, default_value_t = false)]
    pub explain_confidence: bool,

    /// Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed
    /// toon. `json`: full-fidelity JSON. `text`: pretty-printed JSON. Every impact row
    /// carries `requires_verification`: true when an edge on its path is heuristic or
    /// below 0.7 confidence (the row keeps its place). `ownerClass` is the stored
    /// owner, null for a nested fn or closure. `lexicalParent` is the enclosing chain (`K::m`),
    /// else the stored owner. `unexplained_refs` is present when the text scan ran (see
    /// --ambiguous-callers). `blind_spot_warning.files` is an array: upstream, the files of
    /// the `unexplained_refs` sites; downstream, the target's own file. An ambiguous target
    /// exits non-zero and prints `status: "ambiguous"` with at most 5 `candidates`, most
    /// `callers` first, each with `target`, `file`, `line`, `kind`, `callers` and
    /// `top_callers`; `more` and `more_command` name the candidates not shown.
    #[arg(long)]
    pub format: Option<OutputFormat>,

    /// List sites of a path-shaped string literal by exact value.
    /// Mutually exclusive with --target/--baseline/<name>. Returns JSON
    /// with each site's file, line, enclosing fn, and sink classification
    /// (`sink:read` / `sink:write` / `sink:open-read` / `sink:join` / etc).
    /// Designed for LLM split-brain queries: `ecp impact --literal
    /// session_meta.json` answers "where is this file read or written?"
    /// without writing cypher.
    #[arg(long = "literal", value_name = "VALUE", conflicts_with_all = ["name", "target", "baseline"])]
    pub literal: Option<String>,

    /// Auto-detect likely path-literal split-brain pairs across all
    /// PathLiteral nodes. Conservative: same extension, similar basename,
    /// nearby directories, and read-only vs write-only sink separation.
    #[arg(long = "literal-coherence", conflicts_with_all = ["name", "target", "baseline", "literal", "batch"])]
    pub literal_coherence: bool,

    /// Read target symbol names from stdin (one per line; `#` and blank lines
    /// skipped). The graph is loaded once and N symbols are resolved
    /// sequentially — amortises mmap + process spawn across queries.
    /// Each result is prefixed by `=== target: <name> ===` so callers can
    /// split the stream unambiguously. Flags like --direction / --depth /
    /// --exclude-tests apply uniformly to all targets.
    ///
    /// Symbol-mode only: `--batch` combined with `--baseline` or `--literal`
    /// is rejected as an invalid argument. A positional name is also rejected
    /// — stdin is the single source of targets, so a positional would be
    /// silently ignored otherwise.
    #[arg(long, conflicts_with_all = ["name", "baseline", "literal", "literal_coherence"])]
    pub batch: bool,

    /// Force the `unexplained_refs` text scan. Without this flag the scan runs
    /// on an upstream or both walk when the target has at most 2 non-test
    /// direct callers, or shares its name with another definition of a
    /// competing kind. The scan reads every tracked file once for whole-word
    /// occurrences of the bare name, except vendored code and build output
    /// (`node_modules/`, `vendor/`, `.venv/`, `site-packages/`, `dist/`,
    /// `build/`, `*.min.js`, lockfiles), copies (`.patch`, `.diff`, `.snap`)
    /// and documents (`.md`, `.rst`, `.txt`), in every language. It lists
    /// each occurrence that no edge into a same-named definition explains, as
    /// `{path, line, form, enclosing}`: `form` is `bare` or `member` for a
    /// call, `reference` for any other use. At most 20 sites, source files
    /// before tests and examples; `total` counts every occurrence and
    /// `files` the files that hold one. A site whose line holds several
    /// occurrences carries `count`. `total: 0` means the scan ran and found
    /// none; `error` says why the scan failed. Sites are text matches, never
    /// merged into the caller list or counts. With `--batch` the scan keeps
    /// its old rule: it runs only with this flag and only for a name with
    /// two or more same-named definitions. Ignored with `--direction down`.
    #[arg(
        long = "ambiguous-callers",
        alias = "ambiguous_callers",
        default_value_t = false
    )]
    pub ambiguous_callers: bool,

    /// Stop the traversal after this many reached nodes. Library-only (no CLI
    /// flag): the CLI's answer must be exhaustive or its caveat would be a lie.
    /// Callers that only need a bounded sample — the peers watcher's SOFT
    /// cache — set it so a hub symbol cannot materialise six figures of nodes.
    #[arg(skip)]
    pub max_results: Option<usize>,
}

impl ImpactArgs {
    /// Whether the walk lists test nodes (in a test file, or flagged as a test by the index). `--test-coverage` needs test
    /// callers to classify coverage, and clap rejects it beside
    /// `--exclude-tests`.
    pub(crate) fn walks_tests(&self) -> bool {
        !self.exclude_tests || self.test_coverage
    }
}

/// Split a comma-separated flag value into a normalized lowercase Vec.
/// Empty / whitespace-only parts are dropped so `--kind ,function,` works.
pub(crate) fn parse_csv_lower(s: Option<&str>) -> Option<Vec<String>> {
    s.map(|raw| {
        raw.split(',')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .collect()
    })
}

/// Hints produced during impact computation and routed by `run`: stderr
/// nudges for the human/agent reading the terminal, plus a payload caveat.
/// Library callers via `build_payload` stay stderr-clean.
#[derive(Default)]
struct ImpactHints {
    empty_hint_name: Option<String>,
    /// The empty-hint target is a field (Property); the hint adds the
    /// field-read-coverage caveat.
    empty_hint_is_field: bool,
    /// If > 0, emit the hidden-edges footer.
    hidden_edges: u64,
    /// Heuristic edges hidden by the is_heuristic() filter (T-H1).
    hidden_heuristic_edges: u64,
    /// Test callers dropped by `--exclude-tests`.
    hidden_test_callers: u64,
    /// Payload caveat, merged with `Engine::caveat_for` into the `result`
    /// field by `run`. Symbol mode: the target name collides with other
    /// definitions, so bare calls were Tier-3-suppressed at index time and
    /// the caller set is a lower bound. Baseline mode: changed files over
    /// the size cap were not compared.
    payload_caveat: Option<String>,
    /// The text scan's `total`, or why it failed; `None` when it did not run.
    unexplained_refs: Option<Result<u32, String>>,
}

pub fn run(args: ImpactArgs, engine: &Engine) -> Result<(), EcpError> {
    if args.batch {
        return run_batch(args, engine);
    }
    let format = args.format.unwrap_or_default();
    if args.literal_coherence {
        let payload = build_literal_coherence_payload(engine)?;
        return emit_with_caveat(&payload, format, engine.caveat());
    }
    if let Some(literal_value) = args.literal.clone() {
        let payload = literal::build_literal_payload(&literal_value, engine)?;
        return emit_with_caveat(&payload, format, engine.caveat());
    }
    let (payload, hints) = match build_payload_with_hints(&args, engine) {
        // Exit non-zero (MCP marks `isError`), but give the candidates and the
        // caveat on stdout too: an MCP client reads stdout only.
        Err(e @ EcpError::AmbiguousSymbol { .. }) => {
            let target = args.name.as_deref().or(args.target.as_deref());
            let failure = unresolved_payload(&args, target.unwrap_or_default(), &e, engine);
            let caveat = impact_caveat(&args, engine, &failure);
            emit_with_caveat(&failure, format, caveat)?;
            return Err(e);
        }
        other => other?,
    };
    if let Some(name) = &hints.empty_hint_name {
        // Each conclusion cites the scan that backs it, never a guess.
        match (hints.hidden_test_callers, &hints.unexplained_refs) {
            (hidden @ 1.., _) => eprintln!(
                "→ \"{name}\" has no non-test callers; its {hidden} test callers are hidden by --exclude-tests"
            ),
            (0, Some(Ok(0))) => eprintln!(
                "→ \"{name}\" has 0 resolved callers and no text reference outside its definition. Possible: entry point, dead code, or a name built at run time (string dispatch, reflection)"
            ),
            (0, Some(Ok(n))) => {
                // The collision caveat is set only when other definitions
                // share the name: the likeliest reason the resolver left a
                // bare call unattributed.
                let shared = match hints.payload_caveat.is_some() {
                    true => "; other definitions share its name",
                    false => "",
                };
                eprintln!(
                    "→ \"{name}\" has 0 resolved callers{shared}, but {n} text references the graph does not explain (`unexplained_refs`). Read them before treating it as dead code"
                )
            }
            (0, Some(Err(reason))) => eprintln!(
                "→ \"{name}\" has 0 resolved callers; the text scan failed ({reason}), so this is no proof of dead code"
            ),
            (0, None) => eprintln!(
                "→ \"{name}\" exists but has 0 incoming references. Possible: entry point, dead code, or recent rename. Try --direction both"
            ),
        }
        if hints.empty_hint_is_field {
            eprintln!(
                "→ \"{name}\" is a field: some languages don't capture field reads yet (JS class fields, Ruby attrs), so empty may mean uncaptured, not unread — grep to confirm"
            );
        }
    }
    emit_hidden_edges_footer(hints.hidden_edges);
    if hints.hidden_test_callers > 0 {
        eprintln!(
            "note: {} test callers hidden (--exclude-tests); drop the flag to see them",
            hints.hidden_test_callers
        );
    }
    if args.no_heuristic && hints.hidden_heuristic_edges > 0 {
        eprintln!(
            "note: {} heuristic callers suppressed (--no-heuristic); drop the flag to see them",
            hints.hidden_heuristic_edges
        );
    }
    // A baseline answer is about the dirty diff itself, so every overlay
    // line speaks for it.
    let caveat = match args.baseline {
        Some(_) => engine.caveat(),
        None => impact_caveat(&args, engine, &payload),
    };
    emit_with_caveat(
        &payload,
        format,
        merge_caveats(caveat, hints.payload_caveat),
    )
}

/// The caveat of an impact answer on the target `args` names
/// ([`Engine::impact_caveat_for`]): a line that lists dirty files stays when
/// it names a file of `payload`; the unpaired-declarations line, and a C#
/// call line about the target's bare name, stay whatever the files.
fn impact_caveat(args: &ImpactArgs, engine: &Engine, payload: &Value) -> Option<String> {
    let text = args
        .name
        .as_deref()
        .or(args.target.as_deref())
        .unwrap_or_default();
    let request = target_request(text, None, None);
    let target = request.as_ref().map_or(text, |r| r.name());
    engine.impact_caveat_for(target, result_files(payload))
}

/// The files an impact payload names: its rows, its heuristic callers, its
/// `unexplained_refs` sites and an ambiguity's candidates.
fn result_files(payload: &Value) -> Vec<&str> {
    let lists = [
        (&payload["impact"], "filePath"),
        (&payload["heuristic_callers"], "filePath"),
        (&payload["unexplained_refs"]["sites"], "path"),
        (&payload["candidates"], "file"),
    ];
    let mut files: Vec<&str> = lists
        .into_iter()
        .flat_map(|(list, key)| {
            list.as_array()
                .into_iter()
                .flatten()
                .filter_map(move |row| row[key].as_str())
        })
        .collect();
    files.sort_unstable();
    files.dedup();
    files
}

// ── Batch dispatch ────────────────────────────────────────────────────────────

/// Read target names from stdin, one per line (`#` and blank lines skipped).
///
/// The graph is loaded once (by the caller via `Engine`) and N symbols are
/// resolved sequentially against it — amortises the mmap + process-spawn cost
/// that agents would otherwise pay for each single-target call.
///
/// Output: each target's block is prefixed by `=== target: <name> ===` so
/// callers can split the stream unambiguously regardless of `--format`.
/// Per-target fields are identical to single-target mode (status, target,
/// direction, impact, …). A target that fails to resolve (not found, ambiguous)
/// gets a per-target JSON error entry — the batch never aborts early.
fn run_batch(args: ImpactArgs, engine: &Engine) -> Result<(), EcpError> {
    use std::io::BufRead;

    let format = args.format.unwrap_or_default();

    let stdin = std::io::stdin();
    let targets: Vec<String> = stdin
        .lock()
        .lines()
        .map_while(Result::ok)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .collect();

    if targets.is_empty() {
        eprintln!("→ batch: no targets on stdin (one symbol name per line, `#` for comments)");
        return Ok(());
    }

    let caveat = engine.caveat_reading_view();

    for target_name in &targets {
        println!("=== target: {target_name} ===");

        // Clone flags; substitute this target's name.
        let per_target_args = ImpactArgs {
            name: Some(target_name.clone()),
            target: None,
            baseline: None,
            file: args.file.clone(),
            kind: args.kind.clone(),
            direction: args.direction.clone(),
            depth: args.depth,
            high_trust_only: args.high_trust_only,
            min_confidence: args.min_confidence,
            include_tests: args.include_tests,
            exclude_tests: args.exclude_tests,
            relation_types: args.relation_types.clone(),
            repo: args.repo.clone(),
            test_coverage: args.test_coverage,
            no_heuristic: args.no_heuristic,
            confidence_threshold: args.confidence_threshold,
            explain_confidence: args.explain_confidence,
            format: args.format,
            literal: None,
            literal_coherence: false,
            // Keeps the opt-in text scan (`symbol::scan_runs`).
            batch: true,
            ambiguous_callers: args.ambiguous_callers,
            max_results: args.max_results,
        };

        let payload = match build_payload_with_hints(&per_target_args, engine) {
            Ok((p, hints)) => {
                emit_hidden_edges_footer(hints.hidden_edges);
                let merged = merge_caveats(caveat.clone(), hints.payload_caveat);
                // Inline the caveat into the payload so per-target output is
                // self-contained (same contract as single-target mode's
                // `emit_with_caveat`).
                let mut p = p;
                if let Some(c) = merged {
                    p["result"] = serde_json::json!(c);
                }
                p
            }
            Err(e) => {
                let mut p = unresolved_payload(&per_target_args, target_name, &e, engine);
                // A miss under a hidden set is no proof of absence either.
                if let Some(c) = &caveat {
                    p["result"] = serde_json::json!(c);
                }
                p
            }
        };
        emit_with_caveat(&payload, format, None)?;
    }
    Ok(())
}

/// The payload of a target that did not resolve. An agent branches on
/// `status`: a name with several declarations is not a missing name. An
/// ambiguity lists its `candidates` with their evidence
/// ([`symbol::ambiguous_candidates`]).
fn unresolved_payload(args: &ImpactArgs, target: &str, e: &EcpError, engine: &Engine) -> Value {
    let status = match e {
        EcpError::AmbiguousSymbol { .. } => "ambiguous",
        _ => "not_found",
    };
    let mut p = json!({
        "error": e.to_string(),
        "target": target,
        "status": status,
    });
    let fields = (status == "ambiguous")
        .then(|| ambiguity_of(args, target, engine))
        .flatten();
    if let (Some(fields), Some(obj)) = (fields, p.as_object_mut()) {
        obj.extend(fields);
    }
    p
}

/// The candidate fields of an ambiguous target, resolved again on the error
/// path only: `EcpError` has no room for them, and its callers (`peers plan`,
/// `group impact`) match on it.
fn ambiguity_of(args: &ImpactArgs, target: &str, engine: &Engine) -> Option<Map<String, Value>> {
    let graph = engine.graph().ok()?;
    let request = target_request(target, args.kind.as_deref(), args.file.as_deref()).ok()?;
    let merged = MergedGraph::new(graph, engine.overlay_view());
    match resolve_target(&merged, &request) {
        TargetResolution::Ambiguous(a) => Some(symbol::ambiguous_candidates(
            merged,
            &a,
            request.name(),
            args.repo.as_deref(),
        )),
        _ => None,
    }
}

/// Library API: returns the JSON payload only, dropping stderr hints.
///
/// `run` (binary path) calls `build_payload_with_hints` directly so it can
/// print the hints to stderr, which means this thin wrapper has no in-crate
/// caller and `cargo` flags it as dead. Kept `pub` to mirror the 5-command
/// `build_payload` surface introduced in PR #88 for future library consumers.
#[allow(dead_code)]
pub fn build_payload(args: &ImpactArgs, engine: &Engine) -> Result<Value, EcpError> {
    build_payload_with_hints(args, engine).map(|(v, _)| v)
}

fn build_payload_with_hints(
    args: &ImpactArgs,
    engine: &Engine,
) -> Result<(Value, ImpactHints), EcpError> {
    let has_name = args.name.is_some() || args.target.is_some();
    match (has_name, args.baseline.as_ref()) {
        (true, None) => symbol::impact_by_name(args, engine),
        (false, Some(_)) => baseline::impact_with_baseline(args, engine).map(|(v, caveat)| {
            (
                v,
                ImpactHints {
                    payload_caveat: caveat,
                    ..ImpactHints::default()
                },
            )
        }),
        (false, None) => Err(EcpError::InvalidArgument(
            "impact requires a symbol (positional <name> or --target <name>) or --baseline <ref>"
                .into(),
        )),
        (true, Some(_)) => unreachable!("clap conflicts_with prevents this"),
    }
}

/// Attach the hidden-edge count to the JSON result when filtering actually
/// dropped something. Skipping the field when N=0 keeps default invocations
/// noise-free and lets callers branch on `result.get("hidden_edges")`.
fn attach_hidden_edges(result: &mut Value, hidden_edges: u64) {
    if hidden_edges > 0 {
        result["hidden_edges"] = json!(hidden_edges);
    }
}

fn attach_hidden_test_callers(result: &mut Value, hidden: u64) {
    if hidden > 0 {
        result["hidden_test_callers"] = json!(hidden);
    }
}

/// Attach heuristic-filter fields to the JSON result object.
///
/// `hidden_heuristic_edges` is always written (0 is safe — callers can branch
/// on the field existing). `heuristic_callers` section is always present when
/// `include_heuristic` is true (empty array allowed). Each entry carries the
/// `requires_verification: true` that [`bfs::run_bfs`] sets on every row
/// reached through a heuristic edge, so consumers never mistake a heuristic
/// lead for a deterministic caller.
/// `explain_confidence` block is appended when the flag is set.
fn attach_heuristic_fields(
    result: &mut Value,
    hidden_heuristic_edges: u64,
    heuristic_results: Vec<Value>,
    include_heuristic: bool,
    explain_confidence: bool,
    confidence_threshold: f32,
) {
    result["hidden_heuristic_edges"] = json!(hidden_heuristic_edges);
    let heuristic_reached = heuristic_results.len() as u64;
    if include_heuristic {
        result["heuristic_callers"] = json!(heuristic_results);
    }
    if explain_confidence {
        result["explain_confidence"] = json!({
            "threshold": confidence_threshold,
            "edges_filtered_by_tier": {
                "unknown_tier": heuristic_reached + hidden_heuristic_edges,
            },
        });
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

pub(crate) fn direction_str(dir: &Direction) -> &'static str {
    match dir {
        Direction::Up => "upstream",
        Direction::Down => "downstream",
        Direction::Both => "both",
    }
}
