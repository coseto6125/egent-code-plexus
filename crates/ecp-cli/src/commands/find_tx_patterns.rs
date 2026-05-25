//! `ecp find-transaction-patterns` — heuristic Saga + Outbox detector.
//!
//! ## Saga detection
//!
//! Scans the indexed graph for method name-pairs that follow the Saga
//! compensating-transaction pattern:
//!
//!   `<verb>_<noun>`  ↔  `compensate_<verb>_<noun>` | `undo_<verb>_<noun>` | `rollback_<verb>_<noun>`
//!
//! Both methods must share the same owner class.  An optional `--class <Name>`
//! flag restricts scanning to a single class.
//!
//! ## Outbox detection
//!
//! Detects the Transactional Outbox pattern:
//!
//! 1. Name-scan `Class` / `Struct` nodes for outbox table names matching
//!    `(?i)^(outbox_event|event_outbox|message_outbox)(s|es)?$` (snake_case) or
//!    `OutboxEvent` / `EventOutbox` / `MessageOutbox` (PascalCase variants).
//! 2. Find "outbox writer" functions — callables whose `owner_class` equals an
//!    outbox table name, OR any `Function`/`Method` node reachable via a
//!    `References` in-edge from an outbox table node.
//! 3. BFS (depth ≤ 5) through outgoing `Calls` edges from each writer to find
//!    any `Publishes` edge. A reachable publisher confirms the pattern.
//!
//! ## Confidence formula (Outbox)
//!
//! | Condition                                   | Score |
//! |---------------------------------------------|-------|
//! | Outbox table name matched + publisher found  | 0.75  |
//! | + writer is a method on the outbox class     | 0.80  |
//! | Cap                                          | 0.80  |
//!
//! ## Tier labels (per output discipline)
//!
//! | Confidence   | Tier               |
//! |--------------|--------------------|
//! | < 0.75       | `BLIND_SPOT`       |
//! | 0.75–0.85    | `POSSIBLY_RELATED` |
//!
//! All findings carry `requires_verification: true` and **never enter the graph**.

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

    /// Emit only Saga findings (suppress Outbox).
    #[arg(long, conflicts_with = "outbox_only")]
    pub saga_only: bool,

    /// Emit only Outbox findings (suppress Saga).
    #[arg(long, conflicts_with = "saga_only")]
    pub outbox_only: bool,

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

fn detect_saga_pairs(graph: &ArchivedZeroCopyGraph, class_filter: Option<&str>) -> Vec<SagaPair> {
    let mut pairs: Vec<SagaPair> = Vec::new();
    for edge in graph.edges.iter() {
        if !matches!(edge.rel_type, ArchivedRelType::CompensatedBy) {
            continue;
        }
        let comp_idx = edge.source.to_native() as usize;
        let op_idx = edge.target.to_native() as usize;
        let comp_node = &graph.nodes[comp_idx];
        let op_node = &graph.nodes[op_idx];

        let owner = comp_node.owner_class.resolve(&graph.string_pool);
        if let Some(cf) = class_filter {
            if owner != cf {
                continue;
            }
        }

        let comp_name = comp_node.name.resolve(&graph.string_pool);
        let op_name = op_node.name.resolve(&graph.string_pool);
        let op_file = graph.files[op_node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool);
        let op_line = op_node.span.0.to_native();
        let reason = edge.reason.resolve(&graph.string_pool);
        let calls_back = reason == "saga:calls-back";

        pairs.push(SagaPair {
            operation: format!("{owner}.{op_name}"),
            compensator: format!("{owner}.{comp_name}"),
            file: op_file.to_owned(),
            line: op_line,
            confidence: edge.confidence.to_native(),
            calls_back,
        });
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

// ── Outbox detection ──────────────────────────────────────────────────────────

/// Maximum BFS depth when walking Calls edges from an outbox writer.
const OUTBOX_BFS_DEPTH: usize = 5;

/// Canonical name variants for outbox tables, lower-cased for case-insensitive
/// comparison.  Each entry is a root that may be followed by an optional `s` /
/// `es` suffix (handled below).
const OUTBOX_ROOTS: &[&str] = &["outbox_event", "event_outbox", "message_outbox"];

/// Return `true` if `name` matches the outbox table name pattern
/// (case-insensitive, optional `s`/`es` suffix).
fn is_outbox_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    // Accept snake_case roots directly (with optional plural suffix).
    for &root in OUTBOX_ROOTS {
        if lower == root || lower == format!("{root}s") || lower == format!("{root}es") {
            return true;
        }
    }
    // Accept PascalCase variants by normalising to snake_case prefix equivalents.
    // OutboxEvent → outbox_event, EventOutbox → event_outbox, MessageOutbox → message_outbox
    // We cover these by inserting `_` before each uppercase letter and lower-casing.
    let snake = pascal_to_snake(&lower);
    for &root in OUTBOX_ROOTS {
        if snake == root || snake == format!("{root}s") || snake == format!("{root}es") {
            return true;
        }
    }
    false
}

/// Naïve PascalCase → snake_case conversion (already lower-cased input).
/// Only needed for the boundary detection: `outboxevent` → `outbox_event`.
/// Works by looking for known word boundaries.
fn pascal_to_snake(lower: &str) -> String {
    // Insert underscore between "outbox"/"event"/"message" boundary substrings.
    lower
        .replace("outboxevent", "outbox_event")
        .replace("eventoutbox", "event_outbox")
        .replace("messageoutbox", "message_outbox")
}

/// True for node kinds that represent callable units and can own `Calls` edges.
fn is_callable_kind(k: &ArchivedNodeKind) -> bool {
    matches!(
        k,
        ArchivedNodeKind::Function | ArchivedNodeKind::Method | ArchivedNodeKind::Constructor
    )
}

/// Record for a detected Outbox finding.
struct OutboxPattern {
    /// Name of the matched outbox table/class.
    table_name: String,
    table_file: String,
    table_line: u32,
    /// Name of the function/method writing to the outbox.
    writer_name: String,
    writer_file: String,
    writer_line: u32,
    /// True when writer is a method whose owner_class == table_name.
    writer_is_method_on_table: bool,
    /// Name of the downstream publish function.
    publisher_name: String,
    publisher_file: String,
    publisher_line: u32,
    /// Framework lib string from the Publishes edge reason (e.g. "kafka").
    publisher_lib: String,
    confidence: f32,
}

fn detect_outbox_patterns(graph: &ArchivedZeroCopyGraph) -> Vec<OutboxPattern> {
    // ── Step 1: find all outbox table nodes ───────────────────────────────────
    // outbox_tables: Vec<(node_idx, name, file, line)>
    let mut outbox_tables: Vec<(usize, &str, &str, u32)> = Vec::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        if !matches!(
            node.kind,
            ArchivedNodeKind::Class | ArchivedNodeKind::Struct
        ) {
            continue;
        }
        let name = node.name.resolve(&graph.string_pool);
        if !is_outbox_name(name) {
            continue;
        }
        let file = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool);
        let line = node.span.0.to_native();
        outbox_tables.push((idx, name, file, line));
    }

    if outbox_tables.is_empty() {
        return Vec::new();
    }

    // ── Step 2: find outbox-writing functions ─────────────────────────────────
    // Strategy A: methods whose owner_class matches an outbox table name.
    // Strategy B: callables that have a References in-edge from an outbox table.

    // Build set of outbox table node indices for O(1) lookup.
    let table_idx_set: std::collections::HashSet<usize> =
        outbox_tables.iter().map(|(i, ..)| *i).collect();
    // Build map: outbox_table_name (lowercase) → (idx, name, file, line).
    let table_by_name: std::collections::HashMap<String, (usize, &str, &str, u32)> = outbox_tables
        .iter()
        .map(|&(idx, name, file, line)| (name.to_lowercase(), (idx, name, file, line)))
        .collect();

    // writers: Vec<(writer_node_idx, table_idx, is_method_on_table)>
    let mut writers: Vec<(usize, usize, bool)> = Vec::new();
    // Track (writer_idx, table_idx) pairs to deduplicate.
    let mut writer_seen: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();

    for (node_idx, node) in graph.nodes.iter().enumerate() {
        if !is_callable_kind(&node.kind) {
            continue;
        }

        // Strategy A: owner_class matches an outbox table.
        let owner = node.owner_class.resolve(&graph.string_pool);
        if !owner.is_empty() {
            if let Some(&(tbl_idx, ..)) = table_by_name.get(&owner.to_lowercase()) {
                if writer_seen.insert((node_idx, tbl_idx)) {
                    writers.push((node_idx, tbl_idx, true));
                }
            }
        }

        // Strategy B: incoming References edges from an outbox table.
        if graph.in_offsets.len() > node_idx + 1 {
            let in_start = graph.in_offsets[node_idx].to_native() as usize;
            let in_end = graph.in_offsets[node_idx + 1].to_native() as usize;
            for i in in_start..in_end {
                let edge_idx = graph.in_edge_idx[i].to_native() as usize;
                let edge = &graph.edges[edge_idx];
                if !matches!(edge.rel_type, ArchivedRelType::References) {
                    continue;
                }
                let src_idx = edge.source.to_native() as usize;
                if !table_idx_set.contains(&src_idx) {
                    continue;
                }
                if writer_seen.insert((node_idx, src_idx)) {
                    writers.push((node_idx, src_idx, false));
                }
            }
        }
    }

    if writers.is_empty() {
        return Vec::new();
    }

    // ── Step 3: BFS from each writer through Calls edges to find publishers ───

    // Pre-build: for each node, what are its outgoing Calls targets?
    // (We use out_offsets already — just need to filter by Calls.)

    // Also find all Publishes edges: source node → publisher node.
    // publisher_map: node_idx → (target_topic_idx, lib_str)
    // We want: if a node has a Publishes outgoing edge, it IS a publisher.
    // For the output we want the enclosing function name + file + line.
    let mut publisher_nodes: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut publisher_lib: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    for edge in graph.edges.iter() {
        if matches!(edge.rel_type, ArchivedRelType::Publishes) {
            let src = edge.source.to_native() as usize;
            publisher_nodes.insert(src);
            let reason = edge.reason.resolve(&graph.string_pool);
            publisher_lib
                .entry(src)
                .or_insert_with(|| reason.to_owned());
        }
    }

    let mut findings: Vec<OutboxPattern> = Vec::new();
    // Deduplicate (writer_idx, publisher_idx) pairs.
    let mut finding_seen: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();

    for (writer_idx, tbl_idx, is_method_on_table) in &writers {
        // BFS through Calls edges.
        let mut queue: std::collections::VecDeque<(usize, usize)> =
            std::collections::VecDeque::new();
        let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
        queue.push_back((*writer_idx, 0));
        visited.insert(*writer_idx);

        while let Some((curr_idx, depth)) = queue.pop_front() {
            // Check if this node is a publisher.
            // Note: curr_idx == writer_idx is valid — the writer may directly
            // publish (e.g. save() calls producer.send() inline).
            if publisher_nodes.contains(&curr_idx) && finding_seen.insert((*writer_idx, curr_idx)) {
                let &(tbl_node_idx, tbl_name, tbl_file, tbl_line) =
                    outbox_tables.iter().find(|(i, ..)| i == tbl_idx).unwrap();
                let _ = tbl_node_idx;
                let writer_node = &graph.nodes[*writer_idx];
                let writer_name = writer_node.name.resolve(&graph.string_pool);
                let writer_file = graph.files[writer_node.file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.string_pool);
                let writer_line = writer_node.span.0.to_native();
                let pub_node = &graph.nodes[curr_idx];
                let pub_name = pub_node.name.resolve(&graph.string_pool);
                let pub_file = graph.files[pub_node.file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.string_pool);
                let pub_line = pub_node.span.0.to_native();
                let lib = publisher_lib.get(&curr_idx).cloned().unwrap_or_default();
                let confidence = if *is_method_on_table {
                    0.80_f32
                } else {
                    0.75_f32
                };
                findings.push(OutboxPattern {
                    table_name: tbl_name.to_owned(),
                    table_file: tbl_file.to_owned(),
                    table_line: tbl_line,
                    writer_name: writer_name.to_owned(),
                    writer_file: writer_file.to_owned(),
                    writer_line,
                    writer_is_method_on_table: *is_method_on_table,
                    publisher_name: pub_name.to_owned(),
                    publisher_file: pub_file.to_owned(),
                    publisher_line: pub_line,
                    publisher_lib: lib,
                    confidence,
                });
            }

            if depth >= OUTBOX_BFS_DEPTH {
                continue;
            }
            if graph.out_offsets.len() <= curr_idx + 1 {
                continue;
            }
            let out_start = graph.out_offsets[curr_idx].to_native() as usize;
            let out_end = graph.out_offsets[curr_idx + 1].to_native() as usize;
            for i in out_start..out_end {
                let edge = &graph.edges[i];
                if !matches!(edge.rel_type, ArchivedRelType::Calls) {
                    continue;
                }
                let next = edge.target.to_native() as usize;
                if visited.insert(next) {
                    queue.push_back((next, depth + 1));
                }
            }
        }
    }

    findings
}

fn outbox_pattern_to_json(p: &OutboxPattern) -> Value {
    json!({
        "outbox_table": {
            "name": p.table_name,
            "file": p.table_file,
            "line": p.table_line,
        },
        "writer": {
            "name": p.writer_name,
            "file": p.writer_file,
            "line": p.writer_line,
            "is_method_on_table": p.writer_is_method_on_table,
        },
        "publisher": {
            "name": p.publisher_name,
            "file": p.publisher_file,
            "line": p.publisher_line,
            "lib": p.publisher_lib,
        },
        "confidence": p.confidence,
        "tier": tier_label(p.confidence),
        "requires_verification": true,
    })
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(args: FindTxPatternsArgs, engine: &Engine) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    let saga_pairs: Vec<Value> = if args.outbox_only {
        Vec::new()
    } else {
        let pairs = detect_saga_pairs(graph, args.class.as_deref());
        pairs.iter().map(saga_pair_to_json).collect()
    };

    let outbox_patterns: Vec<Value> = if args.saga_only {
        Vec::new()
    } else {
        let patterns = detect_outbox_patterns(graph);
        patterns.iter().map(outbox_pattern_to_json).collect()
    };

    let saga_count = saga_pairs.len();
    let outbox_count = outbox_patterns.len();

    let result = json!({
        "saga_pairs": saga_pairs,
        "outbox_patterns": outbox_patterns,
        "summary": {
            "saga_count": saga_count,
            "outbox_count": outbox_count,
        },
    });

    emit(&result, format)
}
