use super::Direction;
use crate::commands::format::{kind_to_str, node_kind_to_str, rel_type_to_str};
use crate::commands::symbol_id::resolve_owner_class;
use ecp_core::algorithms::process_trace::is_test_path;
use ecp_core::session::{MergedEdge, MergedGraph, OverlayView};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

/// Display metadata for a merged-space node index (`>= graph.nodes.len()` =
/// overlay virtual node). Cold-path companion to `run_bfs`'s inline emission
/// (which keeps its per-file test-path cache).
pub(super) struct MergedNodeMeta {
    pub(super) uid: u64,
    pub(super) name: String,
    pub(super) kind: &'static str,
    pub(super) file_path: String,
    pub(super) line: u32,
}

pub(super) fn merged_node_meta(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    idx: usize,
) -> MergedNodeMeta {
    let merged = MergedGraph::new(graph, view);
    let node = merged
        .node(idx as u32)
        .expect("merged_node_meta called with an index outside merged space");
    MergedNodeMeta {
        uid: node.uid(),
        name: node.name(&merged).to_string(),
        kind: node_kind_to_str(&node.kind()),
        file_path: node.file_path(&merged).unwrap_or_default().to_string(),
        line: node.start_line(),
    }
}

/// Core BFS over the merged graph (base CSR + optional overlay view) from
/// `start_idx`, which is a MERGED-space index: `< graph.nodes.len()` = base
/// node, above = overlay virtual node.
///
/// Returns `(det_results, heur_results, hidden_conf_edges, hidden_heuristic_edges)`.
/// The start node appears at depth 0 in `det_results`.
///
/// - `det_results`: nodes reached exclusively via deterministic edges.
/// - `heur_results`: nodes reached via a heuristic edge (only populated when
///   `include_heuristic` is true; kept in a separate vec so callers render them
///   in a distinct output section per the T-H1 spec).
/// - `hidden_conf_edges`: edges dropped because confidence < `min_conf`.
/// - `hidden_heuristic_edges`: heuristic edges skipped when `include_heuristic`
///   is false. These are the structural signal surfaced as
///   `hidden_heuristic_edges: N` in the output payload.
///
/// `--include-tests` / `--relation-types` / `min_conf` are applied here;
/// `--kind` / `--file` emission-only filtering is NOT applied here.
///
/// Traversal goes through [`MergedGraph`], which owns the overlay merge —
/// masking, endpoint redirection, and overlay adjacency. This function sees
/// edges, not a base graph and a delta.
///
/// **Invariant:** the deterministic result vec always begins with the start
/// node itself at `depth = 0` (so `len() == 1` means "no neighbours reached").
/// Callers relying on this for orphan-detection (see `impact_with_baseline`'s
/// downstream fallback) MUST be updated if this invariant changes.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_bfs(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    start_idx: usize,
    direction: &Direction,
    max_depth: usize,
    min_conf: f32,
    include_tests: bool,
    rel_filter: &Option<Vec<String>>,
    include_heuristic: bool,
) -> (Vec<Value>, Vec<Value>, u64, u64) {
    // (node_idx, depth, via_edge_info, reached_via_heuristic)
    type ViaEdge = Option<(String, f32)>;
    type Step = (usize, usize, ViaEdge, bool);

    let merged = MergedGraph::new(graph, view);
    let base_len = graph.nodes.len();
    let mut visited = HashSet::new();
    let mut queue: VecDeque<Step> = VecDeque::new();
    let mut det_results: Vec<Value> = Vec::new();
    let mut heur_results: Vec<Value> = Vec::new();
    let mut test_path_cache = HashMap::new();
    let mut hidden_conf_edges: u64 = 0;
    let mut hidden_heuristic_edges: u64 = 0;

    queue.push_back((start_idx, 0, None, false));
    visited.insert(start_idx);

    while let Some((curr_idx, curr_depth, via, via_heuristic)) = queue.pop_front() {
        // ── node emission (merged space: < base_len = base, else virtual) ──
        let (uid, name, owner_class, kind_str, file_path, line): (
            u64,
            String,
            Option<String>,
            &'static str,
            String,
            u32,
        );
        if curr_idx < base_len {
            let curr_node = &graph.nodes[curr_idx];
            // BFS via `Decorates` edges can reach synthetic Annotation nodes
            // (SYNTHETIC_FILE_IDX); they have no file:line to report.
            if !curr_node.has_owning_file() {
                continue;
            }
            let file_idx = curr_node.file_idx.to_native() as usize;
            if !include_tests {
                let is_test = *test_path_cache.entry(file_idx).or_insert_with(|| {
                    let file_path = graph.files[file_idx].path.resolve(&graph.string_pool);
                    is_test_path(file_path)
                });
                if is_test {
                    continue;
                }
            }
            uid = curr_node.uid.to_native();
            name = curr_node.name.resolve(&graph.string_pool).to_string();
            owner_class = resolve_owner_class(graph, curr_idx).map(str::to_owned);
            kind_str = kind_to_str(&curr_node.kind);
            file_path = graph.files[file_idx]
                .path
                .resolve(&graph.string_pool)
                .to_string();
            line = curr_node.start_line();
        } else {
            // No has_owning_file guard here: virtual nodes come from freshly
            // parsed source files, never from synthetic emission.
            let vn = view
                .and_then(|v| v.node(curr_idx as u32))
                .expect("virtual index enqueued without a view");
            if !include_tests && is_test_path(&vn.rel_path) {
                continue;
            }
            uid = vn.uid;
            name = vn.name.clone();
            owner_class = vn.owner_class.clone();
            kind_str = node_kind_to_str(&vn.kind);
            file_path = vn.rel_path.to_string();
            line = vn.start_line;
        }

        let (via_reason, via_confidence) = via
            .as_ref()
            .map(|(r, c)| (r.as_str(), *c))
            .unwrap_or(("", 1.0));
        let entry = json!({
            "uid": uid.to_string(),
            "name": name,
            "ownerClass": owner_class,
            "kind": kind_str,
            "filePath": file_path,
            "line": line,
            "depth": curr_depth,
            "viaReason": via_reason,
            "viaConfidence": via_confidence,
        });
        if via_heuristic {
            heur_results.push(entry);
        } else {
            det_results.push(entry);
        }

        if curr_depth >= max_depth {
            continue;
        }

        // ── expansion ───────────────────────────────────────────────────
        // Shared filter chain + enqueue for base and overlay edges.
        let mut consider = |edge: &MergedEdge<'_>, next_idx: usize| {
            let edge_conf = edge.confidence();
            if edge_conf < min_conf {
                hidden_conf_edges += 1;
                return;
            }
            let rel = edge.rel_type();
            // Structural containment edges (Defines, HasMethod, HasProperty,
            // Imports) describe where a symbol lives, not who calls it.
            // Exclude from BFS so File→Function Defines does not register
            // as a caller.
            if rel.is_scope_containment() {
                return;
            }
            let is_heur = rel.is_heuristic();
            if is_heur && !include_heuristic {
                hidden_heuristic_edges += 1;
                return;
            }
            if let Some(rels) = rel_filter.as_ref() {
                let rel_str = rel_type_to_str(rel);
                if !rels.iter().any(|r| r == rel_str) {
                    return;
                }
            }
            if !visited.contains(&next_idx) {
                visited.insert(next_idx);
                queue.push_back((
                    next_idx,
                    curr_depth + 1,
                    Some((edge.reason(graph).to_string(), edge_conf)),
                    is_heur,
                ));
            }
        };

        if matches!(direction, Direction::Up | Direction::Both) {
            for edge in merged.in_edges(curr_idx as u32) {
                let source = edge.source as usize;
                consider(&edge, source);
            }
        }

        if matches!(direction, Direction::Down | Direction::Both) {
            for edge in merged.out_edges(curr_idx as u32) {
                let target = edge.target as usize;
                consider(&edge, target);
            }
        }
    }

    (
        det_results,
        heur_results,
        hidden_conf_edges,
        hidden_heuristic_edges,
    )
}
