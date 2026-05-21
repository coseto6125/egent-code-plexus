//! `ecp find-transaction-patterns` — heuristic Saga compensate-pair detector.
//!
//! ## What this does
//!
//! Scans the indexed graph for method name-pairs that follow the Saga
//! compensating-transaction pattern:
//!
//!   `<verb>_<noun>`  ↔  `compensate_<verb>_<noun>` | `undo_<verb>_<noun>` | `rollback_<verb>_<noun>`
//!
//! Both methods must share the same owner class.  An optional `--class <Name>`
//! flag restricts scanning to a single class.
//!
//! ## Confidence formula
//!
//! | Condition                                          | Score |
//! |----------------------------------------------------|-------|
//! | Exactly one matching compensator on the same class | 0.6   |
//! | + compensator body has a Calls edge to operation   | 0.8   |
//! | Cap                                                | 0.85  |
//!
//! ## Tier labels (per output discipline)
//!
//! | Confidence   | Tier              |
//! |--------------|-------------------|
//! | < 0.75       | `BLIND_SPOT`      |
//! | 0.75–0.85    | `POSSIBLY_RELATED` |
//!
//! All findings carry `requires_verification: true` and **never enter the graph**.
//!
//! ## TODO (future maintainer)
//!
//! `outbox_patterns` detection is intentionally left unimplemented.  It depends
//! on the `EventTopicMirror` heuristic edge type surfaced by T5-33
//! (`EventTopicMirror` / `TransactionScope` schema work).  Once T5-33 lands,
//! implement `detect_outbox_patterns` using `ArchivedRelType::EventTopicMirror`
//! edges on `TransactionScope` nodes and wire it into `run`.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::graph::{ArchivedNodeKind, ArchivedRelType, ArchivedZeroCopyGraph};
use ecp_core::EcpError;
use serde_json::{json, Value};

#[derive(Args, Debug, Clone)]
pub struct FindTxPatternsArgs {
    /// Restrict scan to a single class by name. Omit to scan all classes.
    #[arg(long = "class", value_name = "NAME")]
    pub class: Option<String>,

    /// Repository selector.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format.
    #[arg(long)]
    pub format: Option<String>,
}

// ── Tier helpers ──────────────────────────────────────────────────────────────

fn tier_label(confidence: f32) -> &'static str {
    if confidence >= 0.75 {
        "POSSIBLY_RELATED"
    } else {
        "BLIND_SPOT"
    }
}

// ── Core detection ────────────────────────────────────────────────────────────

/// Record describing a detected Saga pair before JSON serialisation.
struct SagaPair {
    operation: String,
    compensator: String,
    file: String,
    line: u32,
    confidence: f32,
    calls_back: bool,
}

/// Compensator prefixes that form a valid Saga name-pair.
const COMPENSATOR_PREFIXES: &[&str] = &["compensate_", "undo_", "rollback_"];

/// Strip a compensator prefix from `name` and return the bare verb_noun suffix.
/// Returns `None` if `name` does not start with any known prefix.
fn strip_compensator_prefix(name: &str) -> Option<&str> {
    COMPENSATOR_PREFIXES
        .iter()
        .find_map(|&pfx| name.strip_prefix(pfx))
}

/// Check whether `compensator_idx` has a Calls edge pointing at `operation_idx`.
fn compensator_calls_operation(
    graph: &ArchivedZeroCopyGraph,
    compensator_idx: usize,
    operation_idx: usize,
) -> bool {
    let out_start = graph.out_offsets[compensator_idx].to_native() as usize;
    let out_end = graph.out_offsets[compensator_idx + 1].to_native() as usize;
    for i in out_start..out_end {
        let edge = &graph.edges[i];
        if matches!(edge.rel_type, ArchivedRelType::Calls)
            && edge.target.to_native() as usize == operation_idx
        {
            return true;
        }
    }
    false
}

fn detect_saga_pairs(graph: &ArchivedZeroCopyGraph, class_filter: Option<&str>) -> Vec<SagaPair> {
    // Build a lookup: owner_class → Vec<(node_idx, name, file, line)>
    // for nodes that are Method or Function kind.
    let method_kinds =
        |k: &ArchivedNodeKind| matches!(k, ArchivedNodeKind::Method | ArchivedNodeKind::Function);

    // Collect per-class method lists.
    let mut class_methods: std::collections::HashMap<&str, Vec<(usize, &str, &str, u32)>> =
        std::collections::HashMap::new();

    for (idx, node) in graph.nodes.iter().enumerate() {
        if !method_kinds(&node.kind) {
            continue;
        }
        let owner = node.owner_class.resolve(&graph.string_pool);
        if owner.is_empty() {
            continue;
        }
        if let Some(cf) = class_filter {
            if owner != cf {
                continue;
            }
        }
        let name = node.name.resolve(&graph.string_pool);
        let file = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool);
        let line = node.span.0.to_native();
        class_methods
            .entry(owner)
            .or_default()
            .push((idx, name, file, line));
    }

    let mut pairs: Vec<SagaPair> = Vec::new();

    for (class_name, methods) in &class_methods {
        // Build a name → (idx, file, line) map for O(1) lookup of operations.
        let name_map: std::collections::HashMap<&str, (usize, &str, u32)> = methods
            .iter()
            .map(|&(idx, name, file, line)| (name, (idx, file, line)))
            .collect();

        for &(comp_idx, comp_name, _comp_file, _comp_line) in methods {
            let Some(suffix) = strip_compensator_prefix(comp_name) else {
                continue;
            };
            // The suffix is the bare verb_noun; look for an operation with that exact name.
            let Some(&(op_idx, op_file, op_line)) = name_map.get(suffix) else {
                continue;
            };

            let calls_back = compensator_calls_operation(graph, comp_idx, op_idx);
            let confidence = if calls_back { 0.8_f32 } else { 0.6_f32 };
            // Cap enforced here even though current logic can't exceed 0.8.
            let confidence = confidence.min(0.85);

            pairs.push(SagaPair {
                operation: format!("{class_name}.{suffix}"),
                compensator: format!("{class_name}.{comp_name}"),
                file: op_file.to_owned(),
                line: op_line,
                confidence,
                calls_back,
            });
        }
    }

    pairs
}

fn saga_pair_to_json(p: &SagaPair) -> Value {
    json!({
        "operation": p.operation,
        "compensator": p.compensator,
        "file": p.file,
        "line": p.line,
        "confidence": p.confidence,
        "tier": tier_label(p.confidence),
        "evidence": {
            "compensator_calls_operation": p.calls_back,
        },
        "requires_verification": true,
    })
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(args: FindTxPatternsArgs, engine: &Engine) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    let pairs = detect_saga_pairs(graph, args.class.as_deref());
    let saga_count = pairs.len();
    let saga_pairs: Vec<Value> = pairs.iter().map(saga_pair_to_json).collect();

    // outbox_patterns is intentionally empty — see module-level TODO.
    let result = json!({
        "saga_pairs": saga_pairs,
        "outbox_patterns": [],
        "summary": {
            "saga_count": saga_count,
            "outbox_count": 0,
            "outbox_status": "blocked_on_t5_33",
        },
    });

    emit(&result, format)
}
