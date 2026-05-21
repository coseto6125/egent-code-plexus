//! `SchemaField` Node emission + `MirrorsField` heuristic edge bucketing
//! (T4-7).
//!
//! Bridges the gap between per-file `RawSchemaField` (T4-1..T4-6 detectors)
//! and the queryable graph layer: each `RawSchemaField` becomes a
//! `NodeKind::SchemaField` Node connected to its owning class via
//! `HasProperty`, and cross-framework mirrors (e.g. Pydantic `User.email`
//! vs SQLAlchemy `User.email`) are linked by the heuristic `MirrorsField`
//! edge.
//!
//! ## Algorithm
//!
//! 1. **Promote**: iterate `LocalGraph.schema_fields`, emit one
//!    `SchemaField` Node + one `HasProperty` edge (Class → SchemaField)
//!    per field. SymbolTable lookup resolves the owning class name in
//!    the same file; misses silently drop the field (no false-positive
//!    edge to a wrong class).
//!
//! 2. **Bucket**: group emitted SchemaField Nodes by
//!    `(name.to_lowercase(), SchemaType)` so case-variants and
//!    type-compatible mirrors share a bucket.
//!
//! 3. **Pair within bucket**: apply the 4-point strict rubric per pair —
//!    exact case-sensitive name, identical `SchemaType` (granted by
//!    bucket key), identical owner-class name, and bidirectional top-1.
//!    For k=2 the top-1 check is trivial; for k≥3 with a uniform
//!    `(name, type, owner_class)` triple, D3 cluster semantics emit all
//!    `k×(k−1)/2` pairs at confidence 0.9. Different owner-class within
//!    the same bucket is currently dropped (BlindSpot emission is a
//!    documented follow-up — see `test_partial_match_emits_blindspot`).
//!
//! ## Perf
//!
//! - O(N) bucket build; O(k²) pairwise emission where k = #fields sharing
//!   `(name_lc, type)`. Typical k < 10 (real corpora rarely have more
//!   than a handful of cross-framework mirrors per identifier).
//! - Offline-only — runs once at build time, never on `ecp` hot paths.

use crate::resolution::index::SymbolTable;
use ecp_core::analyzer::types::{LocalGraph, SchemaType};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use ecp_core::uid;
use rustc_hash::FxHashMap;

/// Promote `RawSchemaField`s to `SchemaField` Nodes + emit `HasProperty`
/// connections + `MirrorsField` heuristic edges. Returns the count of
/// emitted MirrorsField edges (HasProperty + SchemaField Node counts are
/// derivable from `nodes.len()` delta; this return is for telemetry).
///
/// `file_node_count_before` is the size of `nodes` BEFORE the File-node
/// append loop runs — used to bound SchemaField nodes to the
/// raw-symbols-and-extras region so `file_node_idx` registration further
/// downstream still hits the correct File-node range.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> usize {
    let reason_has_property = string_pool.add("post_process:schema_field:has_property");
    let reason_mirror = string_pool.add("post_process:schema_field:mirrors_field");

    /// Bucket entry: (node_idx, owner_class, exact_name).
    type BucketEntry<'a> = (u32, &'a str, &'a str);
    let mut buckets: FxHashMap<(String, SchemaType), Vec<BucketEntry<'_>>> = FxHashMap::default();

    // Phase 1 — emit SchemaField Nodes + HasProperty edges, populate buckets.
    //
    // Skip `LocalGraph`s with no `schema_fields` (the majority — most files
    // carry no ORM/schema surface). Per-file Owner-class lookup uses
    // SymbolTable's per-file index; misses (e.g. extractor emitted a class
    // name that SymbolTable doesn't know about, perhaps due to file
    // boundary edge cases) silently drop the field. No fabricated edges.
    for (lg_idx, local_graph) in local_graphs.iter().enumerate() {
        let Some(ref schema_fields) = local_graph.schema_fields else {
            continue;
        };
        if schema_fields.is_empty() {
            continue;
        }
        let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
        let file_idx = lg_idx as u32;

        for raw_sf in schema_fields.iter() {
            let owner_name = &*raw_sf.owner_class;
            let field_name = &*raw_sf.name;

            // Resolve the owning class to an existing Node idx. If the
            // SymbolTable doesn't know `owner_name` in this file, the
            // class was not parsed as a `Class` / `Struct` / `Trait` /
            // `Interface` — likely a generated-code or DSL pattern that
            // we don't model. Silently skip rather than emit dangling
            // HasProperty.
            let Some(class_idx) = symbol_table.lookup_in_file(&path_str, owner_name) else {
                continue;
            };

            // UID: T1-5 canonical xxh3-64 over (kind, path, owner, name).
            // owner_class is included so cross-framework mirrors with the same
            // (owner, name) but different SchemaField nodes get distinct UIDs
            // via path / owner disambiguation upstream.
            let node_uid = uid::compute(
                NodeKind::SchemaField,
                &path_str,
                Some(owner_name),
                field_name,
            );
            let name_ref = string_pool.add(field_name);
            let owner_ref = string_pool.add(owner_name);
            let sf_idx = nodes.len() as u32;
            nodes.push(Node {
                uid: node_uid,
                name: name_ref,
                file_idx,
                kind: NodeKind::SchemaField,
                span: raw_sf.span,
                community_id: 0,
                owner_class: owner_ref,
                content_hash: 0,
            });

            // HasProperty: <Class> -> <SchemaField>. Non-heuristic
            // (extractor saw an actual `name: T` / `Column(T)` form, so
            // the class-owns-this-field claim is structural, not inferred).
            edges.push(Edge {
                source: class_idx,
                target: sf_idx,
                rel_type: RelType::HasProperty,
                confidence: 1.0,
                reason: reason_has_property,
            });

            // Bucket key: (lowercase_name, type). Lowercase normalizes
            // `email` vs `Email`; type-class match keeps `email: str`
            // from binding to `email: int`.
            buckets
                .entry((field_name.to_ascii_lowercase(), raw_sf.type_class))
                .or_default()
                .push((sf_idx, owner_name, field_name));
        }
    }

    // Phase 2 — pairwise MirrorsField emission within each bucket.
    //
    // Rubric (per spec line 540 + D3 cluster semantics):
    //   - exact case-sensitive name match
    //   - identical SchemaType (granted by bucket key)
    //   - identical owner-class name (e.g. both "User")
    //   - bidirectional top-1 (trivial for k=2; D3 covers k≥3 uniform)
    //
    // Implementation: sub-group bucket by (exact_name, owner_class). Any
    // sub-group of size ≥ 2 satisfies all four points and emits pairwise
    // `MirrorsField` at heuristic confidence (RelType::MirrorsField is
    // listed under `is_heuristic` so default `ecp impact` hides these
    // unless `--show-heuristic` is set).
    //
    // BlindSpot emission for partial matches (3/4) is a documented
    // follow-up — see ignored test `test_partial_match_emits_blindspot`.
    let mut mirror_count = 0usize;
    for entries in buckets.values() {
        if entries.len() < 2 {
            continue;
        }
        // Sub-group by (exact_name, owner_class).
        let mut sub: FxHashMap<(&str, &str), Vec<u32>> = FxHashMap::default();
        for &(idx, owner, name) in entries {
            sub.entry((name, owner)).or_default().push(idx);
        }
        for group in sub.values() {
            if group.len() < 2 {
                continue;
            }
            // Pairwise emit. Deterministic order: idx-ascending pairs
            // (i, j) where i < j by position in `group`. The vec is
            // populated in iteration order from `entries`, which itself
            // mirrors LocalGraph iteration order — stable across runs.
            for i in 0..group.len() {
                for j in (i + 1)..group.len() {
                    edges.push(Edge {
                        source: group[i],
                        target: group[j],
                        rel_type: RelType::MirrorsField,
                        confidence: 0.9,
                        reason: reason_mirror,
                    });
                    mirror_count += 1;
                }
            }
        }
    }

    mirror_count
}
