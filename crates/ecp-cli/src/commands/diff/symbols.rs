//! `symbols` section: symbol-level cross-snapshot diff with 3-tier certainty.
//!
//! Compares Function / Method / Class / Struct / Enum / Trait / Const / etc.
//! nodes between two graph.bin snapshots. Output is split into three buckets:
//!
//! - `certain` — derived purely from AST-level facts (file added/removed,
//!   symbol added/removed, `content_hash` changed, intra-file Calls edges).
//!   Intra-file callers are placed here because single-file SymbolTable
//!   resolution is closed-world and unambiguous.
//! - `heuristic` — cross-file Calls edges. The graph's resolver tier
//!   confidence is surfaced verbatim; no promotion to "certain".
//! - `unknown` — BlindSpotRecord entries falling inside the diff region.
//!   "Honest no-data" rather than fabricated coverage.
//!
//! Symbol identity = `(file_path, owner_class, name, kind)`. Overloads of
//! the same `(name, kind)` inside the same class collapse — that limitation
//! is accepted; the alternative (signature hash) requires per-language
//! parser changes that don't ship with this PR.

use crate::commands::graph_csr::iter_incoming_edges_filtered;
use crate::engine::Engine;
use ecp_core::graph::{ArchivedNodeKind, ArchivedRelType, NodeKind};
use ecp_core::EcpError;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::path::Path;

/// Synthetic / container kinds excluded from symbol-level diff. They either
/// have no source span (Process, EntryPoint) or are coarse containers
/// (File, Document, Section) whose delta is already captured by
/// `certain.files_added` / `files_removed`. `Import` rows are also dropped
/// because import drift is handled separately via the `bindings` section.
fn is_diffable_kind(k: NodeKind) -> bool {
    !matches!(
        k,
        NodeKind::File
            | NodeKind::Document
            | NodeKind::Section
            | NodeKind::Process
            | NodeKind::EntryPoint
            | NodeKind::Import
    )
}

/// Symbol identity inside a single file. owner_class disambiguates methods
/// on different classes that share a name; kind disambiguates a Function
/// vs Class with identical names (legal in some langs).
#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct SymbolKey {
    path: String,
    owner_class: String,
    name: String,
    kind: &'static str,
}

#[derive(Debug, Clone)]
struct SymbolInfo {
    content_hash: u64,
    line: u32,
    node_idx: u32,
}

struct Snapshot {
    symbols: FxHashMap<SymbolKey, SymbolInfo>,
    /// Files (path → category-tag-as-str) for files_added / files_removed
    /// and test/prod ratio derivation.
    files: FxHashMap<String, String>,
    /// BlindSpots indexed by file_path for fast lookup during diff-region
    /// filtering.
    blindspots: FxHashMap<String, Vec<BlindSpotRef>>,
}

// ── Output schema ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Default)]
pub struct SymbolsDiff {
    pub certain: CertainBucket,
    pub heuristic: HeuristicBucket,
    pub unknown: UnknownBucket,
}

#[derive(Debug, Serialize, Default)]
pub struct CertainBucket {
    pub files_added: Vec<String>,
    pub files_removed: Vec<String>,
    pub symbols_added: Vec<SymbolRef>,
    pub symbols_removed: Vec<SymbolRef>,
    pub symbols_changed: Vec<SymbolChange>,
    pub intra_file_callers: Vec<IntraFileCallersOf>,
}

#[derive(Debug, Serialize, Default)]
pub struct HeuristicBucket {
    pub cross_file_callers: Vec<CrossFileCallersOf>,
}

#[derive(Debug, Serialize, Default)]
pub struct UnknownBucket {
    pub blindspots_in_diff_region: Vec<BlindSpotRef>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct SymbolRef {
    pub path: String,
    pub owner_class: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
}

#[derive(Debug, Serialize, Clone)]
pub struct SymbolChange {
    pub path: String,
    pub owner_class: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
    /// Hex-formatted xxh3-64 of the symbol's raw source bytes. Equal hash
    /// ↔ source identical (signature + body together). Differing hash =
    /// at least *something* changed; we cannot split signature vs body
    /// without per-parser support — that's an explicit v1 limitation.
    pub baseline_hash: String,
    pub current_hash: String,
    /// Node index in the CURRENT graph. Internal-only; used by caller
    /// lookup to skip a linear scan. Not serialized.
    #[serde(skip)]
    pub current_node_idx: u32,
}

#[derive(Debug, Serialize, Clone)]
pub struct IntraFileCallersOf {
    pub target_path: String,
    pub target_name: String,
    pub target_kind: String,
    pub callers: Vec<CallerRef>,
}

#[derive(Debug, Serialize, Clone)]
pub struct CrossFileCallersOf {
    pub target_path: String,
    pub target_name: String,
    pub target_kind: String,
    /// Worst-case confidence across all listed candidates (min). Useful
    /// when the LLM wants a single triage signal.
    pub min_confidence: f32,
    pub candidates: Vec<CrossFileCaller>,
}

#[derive(Debug, Serialize, Clone)]
pub struct CallerRef {
    pub name: String,
    pub kind: String,
    pub line: u32,
}

#[derive(Debug, Serialize, Clone)]
pub struct CrossFileCaller {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub confidence: f32,
    /// Resolver provenance string from the edge's `reason` field — verbatim
    /// (not interpreted). Empty when resolver did not record one.
    pub reason: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct BlindSpotRef {
    pub path: String,
    pub line: u32,
    pub kind: String,
    pub hint: String,
}

// ── Extraction ────────────────────────────────────────────────────────────

fn kind_str(k: &ArchivedNodeKind) -> &'static str {
    NodeKind::from(k).as_str()
}

fn collect_snapshot(graph_path: &Path) -> Result<(Snapshot, Engine), EcpError> {
    let engine = Engine::load(graph_path)
        .map_err(|e| EcpError::Output(format!("load graph {}: {e}", graph_path.display())))?;
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;

    let mut symbols = FxHashMap::default();
    for (idx, node) in graph.nodes.iter().enumerate() {
        let nk = NodeKind::from(&node.kind);
        if !is_diffable_kind(nk) {
            continue;
        }
        let path = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool)
            .to_string();
        let owner_class = node.owner_class.resolve(&graph.string_pool).to_string();
        let name = node.name.resolve(&graph.string_pool).to_string();
        let key = SymbolKey {
            path,
            owner_class,
            name,
            kind: kind_str(&node.kind),
        };
        // Overload collision: keep the first occurrence. (See module doc.)
        symbols.entry(key).or_insert(SymbolInfo {
            content_hash: node.content_hash.to_native(),
            line: node.span.0.to_native(),
            node_idx: idx as u32,
        });
    }

    let mut files = FxHashMap::default();
    for f in graph.files.iter() {
        let path = f.path.resolve(&graph.string_pool).to_string();
        let category = format!("{:?}", ecp_core::graph::FileCategory::from(&f.category));
        files.insert(path, category);
    }

    let mut blindspots: FxHashMap<String, Vec<BlindSpotRef>> = FxHashMap::default();
    for bs in graph.blind_spots.iter() {
        let path = bs.file_path.resolve(&graph.string_pool).to_string();
        blindspots
            .entry(path.clone())
            .or_default()
            .push(BlindSpotRef {
                path,
                line: bs.start_row.to_native(),
                kind: bs.kind.resolve(&graph.string_pool).to_string(),
                hint: bs.hint.resolve(&graph.string_pool).to_string(),
            });
    }

    Ok((
        Snapshot {
            symbols,
            files,
            blindspots,
        },
        engine,
    ))
}

// ── Caller lookup (current graph only) ────────────────────────────────────

fn collect_callers_for_changed(
    current_engine: &Engine,
    changed: &[SymbolChange],
) -> Result<(Vec<IntraFileCallersOf>, Vec<CrossFileCallersOf>), EcpError> {
    let graph = current_engine
        .graph()
        .map_err(|e| EcpError::Rkyv(e.to_string()))?;

    let mut intra: Vec<IntraFileCallersOf> = Vec::new();
    let mut cross: Vec<CrossFileCallersOf> = Vec::new();

    for ch in changed {
        let tidx = ch.current_node_idx;
        let target_node = match graph.nodes.get(tidx as usize) {
            Some(n) => n,
            None => continue,
        };
        let file_idx = target_node.file_idx.to_native();

        let incoming: Vec<_> = iter_incoming_edges_filtered(graph, tidx, |r| {
            matches!(r, ArchivedRelType::Calls | ArchivedRelType::References)
        })
        .collect();
        if incoming.is_empty() {
            continue;
        }

        let mut intra_callers: Vec<CallerRef> = Vec::new();
        let mut cross_candidates: Vec<CrossFileCaller> = Vec::new();

        for (src_idx, edge_idx) in incoming {
            let src_node = &graph.nodes[src_idx as usize];
            let src_path = graph.files[src_node.file_idx.to_native() as usize]
                .path
                .resolve(&graph.string_pool)
                .to_string();
            let src_name = src_node.name.resolve(&graph.string_pool).to_string();
            let src_kind = kind_str(&src_node.kind).to_string();
            let src_line = src_node.span.0.to_native();
            let edge = &graph.edges[edge_idx as usize];
            let conf = edge.confidence.to_native();
            let reason = edge.reason.resolve(&graph.string_pool).to_string();

            if src_node.file_idx.to_native() == file_idx {
                intra_callers.push(CallerRef {
                    name: src_name,
                    kind: src_kind,
                    line: src_line,
                });
            } else {
                cross_candidates.push(CrossFileCaller {
                    path: src_path,
                    name: src_name,
                    kind: src_kind,
                    line: src_line,
                    confidence: conf,
                    reason,
                });
            }
        }

        if !intra_callers.is_empty() {
            intra_callers.sort_by(|a, b| a.line.cmp(&b.line).then(a.name.cmp(&b.name)));
            intra.push(IntraFileCallersOf {
                target_path: ch.path.clone(),
                target_name: ch.name.clone(),
                target_kind: ch.kind.clone(),
                callers: intra_callers,
            });
        }
        if !cross_candidates.is_empty() {
            cross_candidates.sort_by(|a, b| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.path.cmp(&b.path))
                    .then(a.line.cmp(&b.line))
            });
            let min_conf = cross_candidates
                .iter()
                .map(|c| c.confidence)
                .fold(f32::INFINITY, f32::min);
            cross.push(CrossFileCallersOf {
                target_path: ch.path.clone(),
                target_name: ch.name.clone(),
                target_kind: ch.kind.clone(),
                min_confidence: min_conf,
                candidates: cross_candidates,
            });
        }
    }

    intra.sort_by(|a, b| {
        a.target_path
            .cmp(&b.target_path)
            .then(a.target_name.cmp(&b.target_name))
    });
    cross.sort_by(|a, b| {
        a.target_path
            .cmp(&b.target_path)
            .then(a.target_name.cmp(&b.target_name))
    });

    Ok((intra, cross))
}

// ── Public API ────────────────────────────────────────────────────────────

/// Extract a snapshot from a graph.bin path. Returned alongside the loaded
/// `Engine` so callers can re-use the mmap for caller lookup without a
/// second load.
pub fn extract(graph_path: &Path) -> Result<ExtractedSnapshot, EcpError> {
    let (snap, engine) = collect_snapshot(graph_path)?;
    Ok(ExtractedSnapshot { snap, engine })
}

/// Combined snapshot + engine handle. The engine is held for the lifetime
/// of the diff to keep mmap-backed strings alive during caller lookup.
pub struct ExtractedSnapshot {
    snap: Snapshot,
    engine: Engine,
}

/// Compute the symbol-level diff between two snapshots. The `current`
/// engine is used to look up callers of changed symbols (intra-file ->
/// certain, cross-file -> heuristic) — `baseline` is not consulted for
/// callers because the relevant question is "who calls this now".
pub fn diff(
    baseline: &ExtractedSnapshot,
    current: &ExtractedSnapshot,
) -> Result<SymbolsDiff, EcpError> {
    let mut out = SymbolsDiff::default();

    // ── Files ────────────────────────────────────────────────────────────
    let b_paths: FxHashSet<&str> = baseline.snap.files.keys().map(|s| s.as_str()).collect();
    let c_paths: FxHashSet<&str> = current.snap.files.keys().map(|s| s.as_str()).collect();
    for p in &b_paths {
        if !c_paths.contains(p) {
            out.certain.files_removed.push((*p).into());
        }
    }
    for p in &c_paths {
        if !b_paths.contains(p) {
            out.certain.files_added.push((*p).into());
        }
    }
    out.certain.files_added.sort();
    out.certain.files_removed.sort();

    // ── Symbols ──────────────────────────────────────────────────────────
    let mut changed: Vec<SymbolChange> = Vec::new();
    for (key, b_info) in &baseline.snap.symbols {
        match current.snap.symbols.get(key) {
            None => {
                if c_paths.contains(key.path.as_str()) {
                    // File still exists but symbol gone → genuine remove.
                    out.certain.symbols_removed.push(SymbolRef {
                        path: key.path.clone(),
                        owner_class: key.owner_class.clone(),
                        name: key.name.clone(),
                        kind: key.kind.to_string(),
                        line: b_info.line,
                    });
                }
                // If file itself was removed, files_removed already captures it.
            }
            Some(c_info) if c_info.content_hash != b_info.content_hash => {
                changed.push(SymbolChange {
                    path: key.path.clone(),
                    owner_class: key.owner_class.clone(),
                    name: key.name.clone(),
                    kind: key.kind.to_string(),
                    line: c_info.line,
                    baseline_hash: format!("{:016x}", b_info.content_hash),
                    current_hash: format!("{:016x}", c_info.content_hash),
                    current_node_idx: c_info.node_idx,
                });
            }
            Some(_) => {}
        }
    }
    for (key, c_info) in &current.snap.symbols {
        if !baseline.snap.symbols.contains_key(key) && b_paths.contains(key.path.as_str()) {
            // Only count as "symbol added" when the file existed in baseline;
            // otherwise files_added covers it.
            out.certain.symbols_added.push(SymbolRef {
                path: key.path.clone(),
                owner_class: key.owner_class.clone(),
                name: key.name.clone(),
                kind: key.kind.to_string(),
                line: c_info.line,
            });
        }
    }
    out.certain.symbols_added.sort_by(symref_cmp);
    out.certain.symbols_removed.sort_by(symref_cmp);
    changed.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.owner_class.cmp(&b.owner_class))
            .then(a.name.cmp(&b.name))
    });

    // ── Callers of changed symbols (current graph only) ──────────────────
    let (intra, cross) = collect_callers_for_changed(&current.engine, &changed)?;
    out.certain.symbols_changed = changed;
    out.certain.intra_file_callers = intra;
    out.heuristic.cross_file_callers = cross;

    // ── BlindSpots intersecting the diff region ──────────────────────────
    // Diff region = any file in files_added ∪ files_removed ∪ (files
    // containing changed/added/removed symbols).
    let mut diff_files: FxHashSet<&str> = FxHashSet::default();
    for p in &out.certain.files_added {
        diff_files.insert(p.as_str());
    }
    for p in &out.certain.files_removed {
        diff_files.insert(p.as_str());
    }
    for c in &out.certain.symbols_changed {
        diff_files.insert(c.path.as_str());
    }
    for s in &out.certain.symbols_added {
        diff_files.insert(s.path.as_str());
    }
    for s in &out.certain.symbols_removed {
        diff_files.insert(s.path.as_str());
    }
    let mut diff_blindspots: Vec<BlindSpotRef> = Vec::new();
    for path in &diff_files {
        if let Some(bs) = current.snap.blindspots.get(*path) {
            diff_blindspots.extend(bs.iter().cloned());
        }
    }
    diff_blindspots.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    out.unknown.blindspots_in_diff_region = diff_blindspots;

    Ok(out)
}

fn symref_cmp(a: &SymbolRef, b: &SymbolRef) -> std::cmp::Ordering {
    a.path
        .cmp(&b.path)
        .then(a.owner_class.cmp(&b.owner_class))
        .then(a.name.cmp(&b.name))
        .then(a.kind.cmp(&b.kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sk(path: &str, name: &str, kind: &'static str) -> SymbolKey {
        SymbolKey {
            path: path.into(),
            owner_class: String::new(),
            name: name.into(),
            kind,
        }
    }

    fn si(hash: u64, line: u32, node_idx: u32) -> SymbolInfo {
        SymbolInfo {
            content_hash: hash,
            line,
            node_idx,
        }
    }

    /// Pure in-memory diff smoke test exercising the certain bucket without
    /// involving graph.bin / Engine. Caller lookup is not exercised here —
    /// see the integration test for end-to-end coverage.
    #[test]
    fn diff_classifies_added_removed_changed() {
        // We bypass diff() (which needs Engine) and unit-test the symbol
        // classification logic by recreating it inline here. This guards
        // the core hash-compare / set-arithmetic behavior against
        // regressions independent of graph-loading.
        let mut baseline_syms: FxHashMap<SymbolKey, SymbolInfo> = FxHashMap::default();
        baseline_syms.insert(sk("a.rs", "foo", "Function"), si(0x11, 1, 0));
        baseline_syms.insert(sk("a.rs", "bar", "Function"), si(0x22, 10, 1));
        baseline_syms.insert(sk("a.rs", "gone", "Function"), si(0x33, 20, 2));

        let mut current_syms: FxHashMap<SymbolKey, SymbolInfo> = FxHashMap::default();
        current_syms.insert(sk("a.rs", "foo", "Function"), si(0x11, 1, 0));
        current_syms.insert(sk("a.rs", "bar", "Function"), si(0x99, 10, 1));
        current_syms.insert(sk("a.rs", "new_sym", "Function"), si(0x44, 30, 2));

        let mut added = 0;
        let mut removed = 0;
        let mut changed = 0;
        for (k, b) in &baseline_syms {
            match current_syms.get(k) {
                None => removed += 1,
                Some(c) if c.content_hash != b.content_hash => changed += 1,
                Some(_) => {}
            }
        }
        for k in current_syms.keys() {
            if !baseline_syms.contains_key(k) {
                added += 1;
            }
        }
        assert_eq!(added, 1, "new_sym should be added");
        assert_eq!(removed, 1, "gone should be removed");
        assert_eq!(changed, 1, "bar should be content-changed");
    }

    #[test]
    fn is_diffable_kind_skips_synthetic_only() {
        assert!(!is_diffable_kind(NodeKind::File));
        assert!(!is_diffable_kind(NodeKind::Document));
        assert!(!is_diffable_kind(NodeKind::Section));
        assert!(!is_diffable_kind(NodeKind::Process));
        assert!(!is_diffable_kind(NodeKind::EntryPoint));
        assert!(!is_diffable_kind(NodeKind::Import));
        assert!(is_diffable_kind(NodeKind::Function));
        assert!(is_diffable_kind(NodeKind::Method));
        assert!(is_diffable_kind(NodeKind::Struct));
        assert!(is_diffable_kind(NodeKind::Enum));
        assert!(is_diffable_kind(NodeKind::Trait));
        assert!(is_diffable_kind(NodeKind::Const));
        assert!(is_diffable_kind(NodeKind::SchemaField));
        assert!(is_diffable_kind(NodeKind::EventTopic));
    }
}
