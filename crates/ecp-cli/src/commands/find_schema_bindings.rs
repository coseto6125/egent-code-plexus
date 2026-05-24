//! `ecp find-schema-bindings <field>` — surface `MirrorsField` heuristic edges
//! and blind-spot candidates (T4-8).
//!
//! ## Tier mapping
//!
//! Confidence on a `MirrorsField` edge is always `0.9` (emitted in
//! `post_process::schema_field_mirrors`). Tier is derived as:
//!
//! - `confidence >= 0.85` → `LIKELY_RELATED`
//! - `0.70 <= confidence < 0.85` → `BLIND_SPOT`
//!
//! ## Check breakdown
//!
//! The current `MirrorsField` edge stores only `confidence` (f32) and
//! `reason` (StrRef, always `"post_process:schema_field:mirrors_field"`).
//! No per-check booleans are recorded on the edge — the rubric is
//! applied at build time without persisting results. This function
//! **infers** check booleans from the connected node pair:
//!
//! - `name`: source.name == target.name (always true for stored edges;
//!   the bucket key guarantees case-insensitive match, and the sub-group
//!   key requires exact-case match before emission)
//! - `type`: stored rubric requires identical `SchemaType` (bucket key),
//!   but `SchemaType` is not persisted on the archived node → inferred `true`
//!   for nodes connected by a `MirrorsField` edge (rubric would have blocked
//!   if it were false)
//! - `class`: source.owner_class == target.owner_class (readable from graph)
//! - `bidir`: pairwise emission at build time guarantees bidirectionality for
//!   k=2 and D3-uniform clusters → inferred `true` for all stored edges
//!
//! Any field where the value is inferred rather than stored is documented
//! here. If the graph format is extended to carry per-check booleans (a
//! follow-up), replace inferred constants with real reads.
//!
//! ## Blind-spot candidates
//!
//! Cross-owner-class `SchemaField` nodes with the same bare name as the query
//! but no outgoing/incoming `MirrorsField` edge are surfaced as
//! `BLIND_SPOT` candidates — they pass 2/4 checks (name + type implied by
//! existence) but fail owner-class and bidir. The tier threshold `<0.85`
//! fits `BLIND_SPOT`; `requires_verification: true` is always set.

use crate::commands::symbol_id::{resolve_owner_class, split_fqn_target};
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::EcpError;

/// Args for `ecp find-schema-bindings`.
#[derive(Args, Debug)]
pub struct FindSchemaBindingsArgs {
    /// Field to query. Accepts `Class.field` (owner-scoped) or bare `field`
    /// (returns all SchemaField nodes with that name across all classes).
    pub field: String,

    /// Repository selector
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: toon (default) | json | text
    #[arg(long)]
    pub format: Option<String>,
}

/// Tier label derived from edge confidence.
fn tier_label(confidence: f32) -> &'static str {
    if confidence >= 0.85 {
        "LIKELY_RELATED"
    } else {
        "BLIND_SPOT"
    }
}

/// Walk the CSR out-edges of `node_idx` and collect all target indices
/// reached via `MirrorsField`.
fn mirrors_targets(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> Vec<(usize, f32)> {
    let out_start = graph.out_offsets[node_idx].to_native() as usize;
    let out_end = graph.out_offsets[node_idx + 1].to_native() as usize;
    (out_start..out_end)
        .filter_map(|i| {
            let edge = &graph.edges[i];
            if matches!(
                edge.rel_type,
                ecp_core::graph::ArchivedRelType::MirrorsField
            ) {
                Some((
                    edge.target.to_native() as usize,
                    edge.confidence.to_native(),
                ))
            } else {
                None
            }
        })
        .collect()
}

/// True if `node_idx` has any `MirrorsField` edge (in or out).
fn has_any_mirror_edge(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> bool {
    // Check outgoing.
    let out_start = graph.out_offsets[node_idx].to_native() as usize;
    let out_end = graph.out_offsets[node_idx + 1].to_native() as usize;
    if (out_start..out_end).any(|i| {
        matches!(
            graph.edges[i].rel_type,
            ecp_core::graph::ArchivedRelType::MirrorsField
        )
    }) {
        return true;
    }
    // Check incoming.
    let in_start = graph.in_offsets[node_idx].to_native() as usize;
    let in_end = graph.in_offsets[node_idx + 1].to_native() as usize;
    (in_start..in_end).any(|i| {
        let edge_idx = graph.in_edge_idx[i].to_native() as usize;
        matches!(
            graph.edges[edge_idx].rel_type,
            ecp_core::graph::ArchivedRelType::MirrorsField
        )
    })
}

/// Build a single mirror entry JSON value for a target SchemaField node.
fn build_mirror_entry(
    graph: &ArchivedZeroCopyGraph,
    target_idx: usize,
    source_owner: Option<&str>,
    confidence: f32,
) -> serde_json::Value {
    let node = &graph.nodes[target_idx];
    let file_node = &graph.files[node.file_idx.to_native() as usize];
    let file_path = file_node.path.resolve(&graph.string_pool);
    let name = node.name.resolve(&graph.string_pool);
    let owner = resolve_owner_class(graph, target_idx);

    // Per-check inferences — see module doc.
    let check_name = true; // exact-case sub-group guarantees this
    let check_type = true; // bucket key (SchemaType) guarantees this; not re-readable from graph
    let check_class = source_owner
        .zip(owner)
        .map(|(s, t)| s == t)
        .unwrap_or(false);
    let check_bidir = true; // pairwise emission ensures bidirectionality

    serde_json::json!({
        "name": name,
        "owner": owner,
        "framework": null, // FrameworkId not persisted on archived Node — see module doc
        "filePath": file_path,
        "line": node.span.0.to_native(),
        "tier": tier_label(confidence),
        "checks": {
            "name": check_name,
            "type": check_type,
            "class": check_class,
            "bidir": check_bidir,
        },
        "requires_verification": true,
    })
}

/// Build a blind-spot candidate entry for a SchemaField node that shares the
/// same bare name as the query but has no `MirrorsField` edge.
fn build_blind_spot_entry(graph: &ArchivedZeroCopyGraph, node_idx: usize) -> serde_json::Value {
    let node = &graph.nodes[node_idx];
    let file_node = &graph.files[node.file_idx.to_native() as usize];
    let file_path = file_node.path.resolve(&graph.string_pool);
    let name = node.name.resolve(&graph.string_pool);
    let owner = resolve_owner_class(graph, node_idx);

    serde_json::json!({
        "name": name,
        "owner": owner,
        "framework": null,
        "filePath": file_path,
        "line": node.span.0.to_native(),
        "tier": "BLIND_SPOT",
        "checks": {
            "name": true,
            "type": true,  // inferred: same bucket key (name_lc, SchemaType) implies same type class
            "class": false,
            "bidir": false,
        },
        "requires_verification": true,
    })
}

pub fn run(args: FindSchemaBindingsArgs, engine: &Engine) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

    let (owner_filter, bare_name) = split_fqn_target(&args.field);

    // Collect all SchemaField nodes matching the query.
    let matching: Vec<usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(idx, node)| {
            if !matches!(node.kind, ecp_core::graph::ArchivedNodeKind::SchemaField) {
                return None;
            }
            if node.name.resolve(&graph.string_pool) != bare_name {
                return None;
            }
            if let Some(owner) = owner_filter {
                if resolve_owner_class(graph, idx)
                    .map(|oc| oc != owner)
                    .unwrap_or(true)
                {
                    return None;
                }
            }
            Some(idx)
        })
        .collect();

    if matching.is_empty() {
        let result = serde_json::json!({
            "status": "not_found",
            "field": args.field,
            "message": format!("No SchemaField nodes found for '{}'.", args.field),
            "mirrors": [],
            "blind_spot_candidates": [],
            "summary": { "mirrors_count": 0, "blind_spot_count": 0 },
        });
        // Non-zero exit for "not found" — caller can detect programmatically.
        emit(&result, format)?;
        std::process::exit(1);
    }

    let mut mirrors: Vec<serde_json::Value> = Vec::new();
    // Track which nodes are reachable as mirror targets from any matched node.
    let mut seen_mirror_targets: std::collections::HashSet<usize> =
        std::collections::HashSet::new();

    for &src_idx in &matching {
        let src_owner = resolve_owner_class(graph, src_idx);
        for (tgt_idx, confidence) in mirrors_targets(graph, src_idx) {
            if seen_mirror_targets.insert(tgt_idx) {
                mirrors.push(build_mirror_entry(graph, tgt_idx, src_owner, confidence));
            }
        }
    }

    // Blind-spot candidates: matched nodes that have NO MirrorsField edges
    // (in or out), not already surfaced as a mirror target, AND there are ≥2
    // matching nodes (a cluster with ≥2 members means a potential pairing
    // that didn't emit an edge — a single isolated node is not a blind-spot).
    let blind_spot_candidates: Vec<serde_json::Value> = if matching.len() >= 2 {
        matching
            .iter()
            .copied()
            .filter(|&idx| !has_any_mirror_edge(graph, idx) && !seen_mirror_targets.contains(&idx))
            .map(|idx| build_blind_spot_entry(graph, idx))
            .collect()
    } else {
        Vec::new()
    };

    let mirrors_count = mirrors.len();
    let blind_spot_count = blind_spot_candidates.len();

    let result = serde_json::json!({
        "field": args.field,
        "mirrors": mirrors,
        "blind_spot_candidates": blind_spot_candidates,
        "summary": {
            "mirrors_count": mirrors_count,
            "blind_spot_count": blind_spot_count,
        },
    });

    emit(&result, format)
}
