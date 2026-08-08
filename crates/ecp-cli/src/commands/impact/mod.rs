mod baseline;
mod bfs;
mod coverage;
mod literal;
pub mod payload;
mod symbol;

pub use baseline::build_baseline_payload;
pub use literal::build_literal_coherence_payload;
pub use payload::{BaselinePayload, ChangedSymbol, ImpactBySymbol, SymbolImpactPayload};
pub use symbol::{run_for_symbol, LocalImpact};

use crate::engine::Engine;
use crate::output::{emit_with_caveat, merge_caveats, OutputFormat};
use clap::{Args, ValueEnum};
use ecp_core::config;
use ecp_core::{EcpError, HIGH_TRUST_CONFIDENCE};
use serde_json::{json, Value};
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

    /// Disambiguate when name has multiple matches: substring on file path.
    /// `--file_path` / `--file-path` stay as aliases for back-compat.
    #[arg(long = "file", alias = "file_path", alias = "file-path")]
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

    /// Coverage gap analysis: for each touched symbol, classify by test-caller
    /// presence (uncovered / partial / covered). Uses FunctionMeta.is_test
    /// flag from per-language extraction. Outputs uncovered symbols first to
    /// support LLM PR review ("X 改了沒測試"). Implies --include-tests during
    /// traversal so test callers are reachable from the walker.
    #[arg(long, aliases = ["test_coverage", "testCoverage"], default_value_t = false)]
    pub test_coverage: bool,

    /// Suppress heuristic callers (MirrorsField, EventTopicMirror) from the
    /// blast radius. Default: heuristic callers ARE shown, in a separate
    /// `heuristic_callers` bucket tagged `requires_verification`. Pass this
    /// flag for a pure-deterministic blast radius.
    #[arg(long, default_value_t = false)]
    pub no_heuristic: bool,

    /// Informational confidence gate — promotes heuristic edges when T4-7/T5-33
    /// emit per-edge tiers. Currently controls the --explain-confidence report.
    #[arg(long, default_value_t = DEFAULT_CONFIDENCE_THRESHOLD)]
    pub confidence_threshold: f32,

    /// Emit explain_confidence block with threshold + per-tier filtered counts.
    #[arg(long, default_value_t = false)]
    pub explain_confidence: bool,

    /// Output format (mostly internal — agent doesn't set this).
    #[arg(long)]
    pub format: Option<String>,

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
    /// --include-tests apply uniformly to all targets.
    ///
    /// Symbol-mode only: `--batch` combined with `--baseline` or `--literal`
    /// is rejected as an invalid argument. A positional name is also rejected
    /// — stdin is the single source of targets, so a positional would be
    /// silently ignored otherwise.
    #[arg(long, conflicts_with_all = ["name", "baseline", "literal", "literal_coherence"])]
    pub batch: bool,

    /// Stop the traversal after this many reached nodes. Library-only (no CLI
    /// flag): the CLI's answer must be exhaustive or its caveat would be a lie.
    /// Callers that only need a bounded sample — the peers watcher's SOFT
    /// cache — set it so a hub symbol cannot materialise six figures of nodes.
    #[arg(skip)]
    pub max_results: Option<usize>,
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
    /// Payload caveat: the target name collides with other definitions, so
    /// bare calls were Tier-3-suppressed at index time and the caller set is
    /// a lower bound. Merged with `Engine::caveat()` into the `result` field
    /// by `run`.
    ambiguity_caveat: Option<String>,
}

pub fn run(args: ImpactArgs, engine: &Engine) -> Result<(), EcpError> {
    if args.batch {
        return run_batch(args, engine);
    }
    let format = OutputFormat::parse(args.format.as_deref());
    if args.literal_coherence {
        let payload = build_literal_coherence_payload(engine)?;
        return emit_with_caveat(&payload, format, engine.caveat());
    }
    if let Some(literal_value) = args.literal.clone() {
        let payload = literal::build_literal_payload(&literal_value, engine)?;
        return emit_with_caveat(&payload, format, engine.caveat());
    }
    let (payload, hints) = build_payload_with_hints(&args, engine)?;
    if let Some(name) = &hints.empty_hint_name {
        eprintln!(
            "→ \"{name}\" exists but has 0 incoming references. Possible: entry point, dead code, or recent rename. Try --direction both / --include-tests"
        );
        if hints.empty_hint_is_field {
            eprintln!(
                "→ \"{name}\" is a field: some languages don't capture field reads yet (JS class fields, Ruby attrs), so empty may mean uncaptured, not unread — grep to confirm"
            );
        }
    }
    emit_hidden_edges_footer(hints.hidden_edges);
    if args.no_heuristic && hints.hidden_heuristic_edges > 0 {
        eprintln!(
            "note: {} heuristic callers suppressed (--no-heuristic); drop the flag to see them",
            hints.hidden_heuristic_edges
        );
    }
    emit_with_caveat(
        &payload,
        format,
        merge_caveats(engine.caveat(), hints.ambiguity_caveat),
    )
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

    let format = OutputFormat::parse(args.format.as_deref());

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

    let caveat = engine.caveat();

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
            relation_types: args.relation_types.clone(),
            repo: args.repo.clone(),
            test_coverage: args.test_coverage,
            no_heuristic: args.no_heuristic,
            confidence_threshold: args.confidence_threshold,
            explain_confidence: args.explain_confidence,
            format: args.format.clone(),
            literal: None,
            literal_coherence: false,
            batch: false,
            max_results: args.max_results,
        };

        let payload = match build_payload_with_hints(&per_target_args, engine) {
            Ok((p, hints)) => {
                emit_hidden_edges_footer(hints.hidden_edges);
                let merged = merge_caveats(caveat.clone(), hints.ambiguity_caveat);
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
                serde_json::json!({
                    "error": e.to_string(),
                    "target": target_name,
                    "status": "not_found",
                })
            }
        };
        emit_with_caveat(&payload, format, None)?;
    }
    Ok(())
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
        (false, Some(_)) => {
            baseline::impact_with_baseline(args, engine).map(|v| (v, ImpactHints::default()))
        }
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

/// Attach heuristic-filter fields to the JSON result object.
///
/// `hidden_heuristic_edges` is always written (0 is safe — callers can branch
/// on the field existing). `heuristic_callers` section is always present when
/// `include_heuristic` is true (empty array allowed), with each entry tagged
/// `requires_verification: true` so consumers never mistake a heuristic lead
/// for a deterministic caller.
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
        result["heuristic_callers"] = json!(tag_heuristic(heuristic_results));
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

/// Tag each heuristic caller `requires_verification: true` so a consumer never
/// mistakes a ~0.85 heuristic lead for a 1.0 deterministic caller.
fn tag_heuristic(entries: Vec<Value>) -> Vec<Value> {
    entries
        .into_iter()
        .map(|mut e| {
            if let Some(obj) = e.as_object_mut() {
                obj.insert("requires_verification".to_string(), json!(true));
            }
            e
        })
        .collect()
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
