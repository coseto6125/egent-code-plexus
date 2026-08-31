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

use crate::commands::impact::bfs::{shortest_path, PathStep};
use crate::commands::impact::{direction_str, parse_csv_lower, Direction};
use crate::commands::symbol_id::resolve_candidates;
use crate::engine::Engine;
use crate::output::{emit_with_caveat, OutputFormat};
use clap::Args;
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Shortest route between two symbols, as an ordered chain with the edge that
/// makes each hop.
///
/// Both names take the bare symbol or the `Owner.Method` FQN form. An
/// overloaded name is not an error: every candidate seeds the same walk, and
/// the answer reports the file:line of the two endpoints it actually joined.
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
    #[arg(long, default_value = "0.0")]
    pub min_confidence: f32,

    /// Allow heuristic edges (MirrorsField, EventTopicMirror) as steps. Each
    /// one is tagged `requires_verification` in the output.
    #[arg(long)]
    pub include_heuristic: bool,

    /// Repository selector.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format (toon / json / text).
    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: PathArgs, engine: &Engine) -> Result<(), EcpError> {
    let (payload, caveat) = build_payload(&args, engine)?;
    emit_with_caveat(
        &payload,
        OutputFormat::parse(args.format.as_deref()),
        caveat,
    )
}

/// Payload plus an optional `result` caveat. Split from `run` so the MCP
/// surface and tests can assert the structure without capturing stdout.
pub fn build_payload(
    args: &PathArgs,
    engine: &Engine,
) -> Result<(Value, Option<String>), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let view = engine.overlay_view();

    let (starts, _) = resolve_candidates(graph, view, &args.from, None, None);
    let (goal_vec, _) = resolve_candidates(graph, view, &args.to, None, None);
    if starts.is_empty() {
        return Err(unresolved(&args.from));
    }
    if goal_vec.is_empty() {
        return Err(unresolved(&args.to));
    }
    let goals: HashSet<usize> = goal_vec.iter().copied().collect();

    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    let found = shortest_path(
        graph,
        view,
        &starts,
        &goals,
        &args.direction,
        args.depth,
        args.min_confidence,
        args.include_tests,
        &rel_filter,
        args.include_heuristic,
    );

    let direction = direction_str(&args.direction);
    let Some(steps) = found else {
        let payload = json!({
            "status": "success",
            "from": args.from,
            "to": args.to,
            "direction": direction,
            "found": false,
            "depth": args.depth,
            "fromCandidates": starts.len(),
            "toCandidates": goals.len(),
        });
        let mut caveat = format!(
            "no {direction} path from '{}' to '{}' within {} hops. Widen with --depth, \
             --direction both, --include-tests or --include-heuristic; an unreachable pair \
             is a real answer, not a missing index.",
            args.from, args.to, args.depth
        );
        // Swapped arguments are the one mistake this command invites, so spend
        // a second walk to turn the dead end into the command that works. Only
        // on a miss, where the first walk already exhausted the reachable set.
        if let Some((flipped, hops)) =
            reverse_probe(args, graph, view, &starts, &goals, &rel_filter)
        {
            caveat.push_str(&format!(
                " There is a path in the other direction ({flipped}, {hops} hops): \
                 rerun with --direction {flipped}."
            ));
        }
        return Ok((payload, Some(caveat)));
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
        "fromCandidates": starts.len(),
        "toCandidates": goals.len(),
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
    Ok((payload, caveat))
}

/// Hops the flipped direction would need, when the requested one found none.
/// `None` for `--direction both`, where there is no other direction to try.
fn reverse_probe(
    args: &PathArgs,
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&ecp_core::session::OverlayView>,
    starts: &[usize],
    goals: &HashSet<usize>,
    rel_filter: &Option<Vec<String>>,
) -> Option<(&'static str, usize)> {
    let flipped = match args.direction {
        Direction::Up => Direction::Down,
        Direction::Down => Direction::Up,
        Direction::Both => return None,
    };
    let steps = shortest_path(
        graph,
        view,
        starts,
        goals,
        &flipped,
        args.depth,
        args.min_confidence,
        args.include_tests,
        rel_filter,
        args.include_heuristic,
    )?;
    Some((direction_str(&flipped), steps.len() - 1))
}

fn step_json(step: &PathStep) -> Value {
    // The start node has no incoming edge; its via fields stay empty rather
    // than borrowing the next hop's, which would misread as a self-edge.
    let (rel, reason, confidence, heuristic) = match &step.via {
        Some(e) => (e.rel, e.reason.as_str(), e.confidence, e.heuristic),
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

fn unresolved(name: &str) -> EcpError {
    EcpError::InvalidArgument(format!(
        "No symbol named '{name}' found in graph. Try `ecp find {name} --mode fuzzy` for candidates"
    ))
}
