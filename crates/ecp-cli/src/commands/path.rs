//! `ecp path` — the concrete chain from one symbol to another.
//!
//! `ecp impact` answers "who does A reach", from a single endpoint. Cypher's
//! `-[:Calls*1..N]->` answers "does A reach B" but throws the route away:
//! `bfs_var_len` in the cypher executor keeps only `(target, last_edge)`, so
//! the intermediate nodes never leave the walk. Neither answers "A reaches B
//! **through what**", which is the question behind "why does changing A break
//! B" and "how does this handler end up touching the database".
//!
//! Without it an LLM reconstructs the chain by guessing which of impact's
//! reached symbols sit between the two — exactly the fabrication the graph
//! exists to prevent.
//!
//! ## Deterministic by default
//!
//! Heuristic edges (MirrorsField, EventTopicMirror) are excluded unless
//! `--include-heuristic` is passed. `ecp impact` can afford to include them
//! because it buckets them separately and tags them `requires_verification`;
//! a path has no second bucket — a step is in the chain or it is not. So the
//! default answer is the one that is safe to act on, and the opt-in flag tags
//! every inferred step it lets through.

use crate::commands::format::node_kind_to_str;
use crate::commands::impact::bfs::{shortest_path, PathStep, WalkOpts};
use crate::commands::impact::{direction_str, parse_csv_lower, Direction};
use crate::commands::symbol_id::same_name_def_count;
use crate::engine::Engine;
use crate::output::{emit_with_caveat, merge_caveats, OutputFormat};
use crate::read_context::{candidate_json, resolve_target, shell_word, target_request};
use clap::Args;
use ecp_core::contracts::identity::{Ambiguity, TargetRequest, TargetResolution};
use ecp_core::session::MergedGraph;
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Shortest route between two symbols, as an ordered chain with the edge that
/// makes each hop.
///
/// Both names take the target grammar of `ecp impact`: `name`, `Owner.name`,
/// `::name` and `name@line`. A name that matches several declarations is an
/// ambiguity: the answer lists each one with the flags that select it.
#[derive(Args, Debug)]
pub struct PathArgs {
    /// Start symbol.
    pub from: String,

    /// Destination symbol.
    pub to: String,

    /// Which way to walk from `from`. `down` follows callees (A calls … calls
    /// B), `up` follows callers, `both` ignores edge direction.
    #[arg(long, value_enum, default_value = "down")]
    pub direction: Direction,

    /// Maximum hops to search.
    #[arg(long, default_value = "8")]
    pub depth: usize,

    /// Comma-separated relation types to follow (calls, extends, ...).
    /// Default: every non-containment relation.
    #[arg(long = "relation_types", alias = "relation-types")]
    pub relation_types: Option<String>,

    /// Include test files in the route. Off by default, so a production path
    /// is not routed through a test helper that happens to call both ends.
    #[arg(long)]
    pub include_tests: bool,

    /// Drop edges below this confidence (0.0–1.0). Default 0.0 is
    /// recall-first, matching `ecp impact`.
    #[arg(long, default_value = "0.0", value_parser = parse_confidence)]
    pub min_confidence: f32,

    /// Select `<FROM>` among several declarations: substring on the file path.
    #[arg(long)]
    pub from_file: Option<String>,

    /// Select `<TO>` among several declarations: substring on the file path.
    #[arg(long)]
    pub to_file: Option<String>,

    /// Select `<FROM>` among several declarations by node kind, for example `method`.
    #[arg(long)]
    pub from_kind: Option<String>,

    /// Select `<TO>` among several declarations by node kind, for example `method`.
    #[arg(long)]
    pub to_kind: Option<String>,

    /// Allow heuristic edges (MirrorsField, EventTopicMirror) as steps. Each
    /// one is tagged `requiresVerification` in the output.
    #[arg(long)]
    pub include_heuristic: bool,

    /// Repository selector.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed
    /// toon. `json`: full-fidelity JSON. `text`: pretty-printed JSON.
    #[arg(long)]
    pub format: Option<OutputFormat>,
}

/// Rejects out-of-range and NaN rather than silently filtering every edge and
/// reporting `found: false` — a miss caused by a bad flag is indistinguishable
/// from a real one in the output, which is the worst kind of wrong answer here.
fn parse_confidence(raw: &str) -> Result<f32, String> {
    let v: f32 = raw
        .parse()
        .map_err(|_| format!("'{raw}' is not a number"))?;
    if !(0.0..=1.0).contains(&v) {
        return Err(format!("must be between 0.0 and 1.0, got '{raw}'"));
    }
    Ok(v)
}

pub fn run(args: PathArgs, engine: &Engine) -> Result<(), EcpError> {
    let format = args.format.unwrap_or_default();
    match build_payload(&args, engine)? {
        Outcome::Answer(payload, caveat) => emit_with_caveat(
            &payload,
            format,
            // The engine's staleness caveat comes first: a `found: false` read off
            // a warm-attached or behind-HEAD graph is provisional, and this payload
            // otherwise states the opposite in as many words.
            merge_caveats(engine.caveat(), caveat),
        ),
        // Exit non-zero (MCP marks `isError`), but give the candidates on stdout
        // too: an MCP client reads stdout only.
        Outcome::Ambiguous(payload, error) => {
            emit_with_caveat(&payload, format, engine.caveat())?;
            Err(error)
        }
    }
}

enum Outcome {
    /// Payload plus an optional `result` caveat.
    Answer(Value, Option<String>),
    /// An endpoint names several declarations: the typed ambiguity payload and
    /// the error that carries the non-zero exit.
    Ambiguous(Value, EcpError),
}

enum Endpoint {
    /// The merged node index, and how many declarations share its name.
    Node(usize, usize),
    Ambiguous(Value, EcpError),
}

/// The one declaration `text` names, through the same resolver as `impact`.
/// `side` is `from` or `to`; it names the flags that select a candidate.
fn endpoint(
    merged: &MergedGraph<'_>,
    text: &str,
    kind: Option<&str>,
    file: Option<&str>,
    side: &str,
) -> Result<Endpoint, EcpError> {
    let request = target_request(text, kind, file)?;
    match resolve_target(merged, &request) {
        TargetResolution::Unique(c) => Ok(Endpoint::Node(
            c.node as usize,
            same_name_def_count(merged, merged.view(), request.name()),
        )),
        TargetResolution::Ambiguous(a) => {
            let payload = json!({
                "status": "ambiguous",
                "error": a.to_error(&request).to_string(),
                "side": side,
                "target": text,
                "candidates": ambiguity_candidates_with_flags(
                    &a, "", &format!("--{side}-file"), &format!("--{side}-kind")),
            });
            Ok(Endpoint::Ambiguous(payload, a.to_error(&request)))
        }
        TargetResolution::NotFound { named } => Err(unresolved(&request, named)),
    }
}

/// The candidates of an ambiguity as `read_context::ambiguity_candidates`
/// reads them, for a command whose filters are not `--file` and `--kind`.
/// `target` is the arguments that select one candidate alone: `text_prefix`,
/// the target text, then the file and kind flags the request needs.
pub(crate) fn ambiguity_candidates_with_flags(
    ambiguity: &Ambiguity,
    text_prefix: &str,
    file_flag: &str,
    kind_flag: &str,
) -> Value {
    let args = |request: &TargetRequest| {
        let mut out = format!("{text_prefix}{}", shell_word(&request.render()));
        if let Some(file) = request.file() {
            out.push_str(&format!(" {file_flag} {}", shell_word(file)));
        }
        if let Some(kind) = request.kind() {
            out.push_str(&format!(
                " {kind_flag} {}",
                node_kind_to_str(&kind).to_ascii_lowercase()
            ));
        }
        out
    };
    ambiguity
        .candidates()
        .iter()
        .map(|c| candidate_json(c, c.reusable.as_ref().map(&args)))
        .collect()
}

/// Kept apart from `run` so the caveat merging and the emit stay one readable
/// line each. MCP reaches this command by spawning the binary, so `run` is
/// the only caller.
fn build_payload(args: &PathArgs, engine: &Engine) -> Result<Outcome, EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let view = engine.overlay_view();
    let merged = MergedGraph::new(graph, view);

    let (from_node, from_defs) = match endpoint(
        &merged,
        &args.from,
        args.from_kind.as_deref(),
        args.from_file.as_deref(),
        "from",
    )? {
        Endpoint::Node(node, defs) => (node, defs),
        Endpoint::Ambiguous(payload, error) => return Ok(Outcome::Ambiguous(payload, error)),
    };
    let (to_node, to_defs) = match endpoint(
        &merged,
        &args.to,
        args.to_kind.as_deref(),
        args.to_file.as_deref(),
        "to",
    )? {
        Endpoint::Node(node, defs) => (node, defs),
        Endpoint::Ambiguous(payload, error) => return Ok(Outcome::Ambiguous(payload, error)),
    };
    let starts = [from_node];
    let mut goals: HashSet<usize> = HashSet::from([to_node]);
    let to_candidates = 1;
    // A construction lands on the type's constructor when it has one, so a
    // route reaching `Foo.__init__` reaches `Foo`. The route still ends at
    // the constructor node it actually reached.
    goals.extend(
        merged
            .constructors_of(to_node as u32)
            .into_iter()
            .map(|c| c as usize),
    );
    let ambiguity = merge_caveats(
        ambiguity_caveat(&args.from, from_defs),
        ambiguity_caveat(&args.to, to_defs),
    );

    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    let opts = WalkOpts {
        direction: args.direction.clone(),
        depth: args.depth,
        min_conf: args.min_confidence,
        include_tests: args.include_tests,
        rel_filter: &rel_filter,
        include_heuristic: args.include_heuristic,
    };
    let found = shortest_path(graph, view, &starts, &goals, &opts);

    let direction = direction_str(&args.direction);
    let Some(steps) = found else {
        let payload = json!({
            "status": "success",
            "from": args.from,
            "to": args.to,
            "direction": direction,
            "found": false,
            "depth": args.depth,
            "fromCandidates": 1,
            "toCandidates": to_candidates,
        });
        let mut caveat = format!(
            "no {direction} path from '{}' to '{}' within {} hops. Widen with --depth, \
             --direction both, --include-tests or --include-heuristic; an unreachable pair \
             is a real answer, not a missing index.",
            args.from, args.to, args.depth
        );
        if args.min_confidence > 0.0 {
            caveat.push_str(&format!(
                " --min-confidence {} is also dropping edges; a miss under a gate is not the \
                 same claim as a miss without one.",
                args.min_confidence
            ));
        }
        // Swapped arguments are the one mistake this command invites, so spend a
        // second walk to turn the dead end into the command that works. Only on
        // a miss, where the first walk already exhausted the reachable set.
        if let Some(flipped) = opts.flipped() {
            if let Some(steps) = shortest_path(graph, view, &starts, &goals, &flipped) {
                let (name, hops) = (direction_str(&flipped.direction), steps.len() - 1);
                caveat.push_str(&format!(
                    " There is a path in the other direction ({name}, {hops} hops): \
                     rerun with --direction {name}."
                ));
            }
        }
        return Ok(Outcome::Answer(
            payload,
            merge_caveats(ambiguity, Some(caveat)),
        ));
    };

    let heuristic_steps = steps
        .iter()
        .filter(|s| s.via.as_ref().is_some_and(|e| e.heuristic))
        .count();
    let payload = json!({
        "status": "success",
        "from": args.from,
        "to": args.to,
        "direction": direction,
        "found": true,
        "hops": steps.len() - 1,
        "fromCandidates": 1,
        "toCandidates": to_candidates,
        "path": steps.iter().map(step_json).collect::<Vec<_>>(),
    });
    let caveat = (heuristic_steps > 0).then(|| {
        format!(
            "{heuristic_steps} of {} hops run along heuristic edges (marked \
             requiresVerification): the route is inferred, not resolved. Re-run without \
             --include-heuristic for the deterministic-only answer.",
            steps.len() - 1
        )
    });
    Ok(Outcome::Answer(payload, merge_caveats(ambiguity, caveat)))
}

/// With two or more same-named definitions in the graph, the resolver may
/// have suppressed bare calls to that name at index time
/// (`DecisionTier::AmbiguousGlobal`), so edges are missing from the walk. A
/// miss under that condition is a lower bound, and this payload otherwise
/// says an unreachable pair is a real answer.
fn ambiguity_caveat(name: &str, same_name_defs: usize) -> Option<String> {
    (same_name_defs >= 2).then(|| {
        format!(
            "route may be incomplete: {same_name_defs} same-named definitions of '{name}' \
             exist in the index or working tree, so bare calls (no import/qualifier context) \
             may be unattributed. Narrow with --from-file / --to-file, and run \
             `ecp impact --target {name} --file <path> --ambiguous-callers` to list the call \
             sites the graph could not attribute before trusting a miss."
        )
    })
}

fn step_json(step: &PathStep) -> Value {
    // The start node has no incoming edge; its via fields stay empty rather
    // than borrowing the next hop's, which would misread as a self-edge.
    let (rel, reason, confidence, heuristic) = match &step.via {
        Some(e) => (e.rel, e.reason, e.confidence, e.heuristic),
        None => ("", "", 1.0, false),
    };
    json!({
        "name": step.meta.name,
        "kind": step.meta.kind,
        "filePath": step.meta.file_path,
        "line": step.meta.line,
        "viaRelType": rel,
        "viaReason": reason,
        "viaConfidence": confidence,
        "requiresVerification": heuristic,
    })
}

fn unresolved(request: &TargetRequest, named: u32) -> EcpError {
    let target = request.render();
    EcpError::InvalidArgument(match named {
        0 => format!(
            "No symbol named '{target}' found in graph. Try `ecp find {} --mode fuzzy` for candidates",
            shell_word(request.name())
        ),
        n => format!(
            "No symbol named '{target}' found in graph: {n} declaration(s) named '{}' exist, \
             but none passes the owner, @line, file or kind filter",
            request.name()
        ),
    })
}
