use super::Direction;
use crate::commands::format::{kind_to_str, node_kind_to_str, rel_to_str};
use crate::commands::symbol_id::resolve_owner_class;
use ecp_core::algorithms::process_trace::is_test_path;
use ecp_core::graph::RelType;
use ecp_core::session::{OverlayView, ViewEdge};
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
    if let Some(vn) = view.and_then(|v| v.node(idx as u32)) {
        return MergedNodeMeta {
            uid: vn.uid,
            name: vn.name.clone(),
            kind: node_kind_to_str(&vn.kind),
            file_path: vn.rel_path.to_string(),
            line: vn.start_line,
        };
    }
    let node = &graph.nodes[idx];
    MergedNodeMeta {
        uid: node.uid.to_native(),
        name: node.name.resolve(&graph.string_pool).to_string(),
        kind: kind_to_str(&node.kind),
        file_path: graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool)
            .to_string(),
        line: node.start_line(),
    }
}

/// One edge under merged traversal: an archived base-graph edge or an
/// overlay-resolved [`ViewEdge`]. Unifies the filter chain (confidence,
/// containment, heuristic, `--relation-types`) so base and overlay edges
/// can never drift on traversal policy.
enum MergedEdgeRef<'a> {
    Base(&'a ecp_core::graph::ArchivedEdge),
    Overlay(&'a ViewEdge),
}

impl MergedEdgeRef<'_> {
    fn confidence(&self) -> f32 {
        match self {
            Self::Base(e) => e.confidence.to_native(),
            Self::Overlay(e) => e.confidence,
        }
    }

    fn rel_type(&self) -> RelType {
        match self {
            Self::Base(e) => RelType::from(&e.rel_type),
            Self::Overlay(e) => e.rel_type,
        }
    }

    fn rel_str(&self) -> &'static str {
        match self {
            Self::Base(e) => rel_to_str(&e.rel_type),
            Self::Overlay(e) => {
                debug_assert!(
                    matches!(e.rel_type, RelType::Calls),
                    "extend rel_str when the overlay gains new edge kinds"
                );
                "calls"
            }
        }
    }

    /// `viaReason` for the BFS payload. Overlay edges carry a static marker
    /// so consumers can tell a caller comes from an uncommitted edit.
    fn reason(&self, graph: &ecp_core::graph::ArchivedZeroCopyGraph) -> String {
        match self {
            Self::Base(e) => e.reason.resolve(&graph.string_pool).to_string(),
            Self::Overlay(_) => "l1-overlay".to_string(),
        }
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
/// With a view, the masking invariant (mask ⊆ rebuild) governs base edges:
/// sourced-in-dirty-file edges are masked only for rels the overlay
/// re-resolves (`Calls` today — overlay adjacency is that file's truth);
/// other rels keep their base edges. Either endpoint in a dirty file is
/// redirected (replaced) into merged space or dropped (suppressed).
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
        let mut consider = |edge: MergedEdgeRef<'_>, next_idx: usize| {
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
                let rel_str = edge.rel_str();
                if !rels.iter().any(|r| r == rel_str) {
                    return;
                }
            }
            if !visited.contains(&next_idx) {
                visited.insert(next_idx);
                queue.push_back((
                    next_idx,
                    curr_depth + 1,
                    Some((edge.reason(graph), edge_conf)),
                    is_heur,
                ));
            }
        };

        if matches!(direction, Direction::Up | Direction::Both) {
            // Base IN-edges anchor: the node itself when base; the replaced
            // base twin when virtual (clean-file callers still point at it);
            // None for a brand-new virtual symbol — no base edge can target
            // it, so only the overlay reverse index below applies.
            let in_anchor = if curr_idx < base_len {
                Some(curr_idx)
            } else {
                view.and_then(|v| v.node(curr_idx as u32))
                    .and_then(|n| n.replaced_base)
                    .map(|b| b as usize)
            };
            if let Some(anchor) = in_anchor {
                let in_start = graph.in_offsets[anchor].to_native() as usize;
                let in_end = graph.in_offsets[anchor + 1].to_native() as usize;
                for i in in_start..in_end {
                    let edge_idx = graph.in_edge_idx[i].to_native() as usize;
                    let edge = &graph.edges[edge_idx];
                    let src = edge.source.to_native() as usize;
                    // mask ⊆ rebuild: drop a dirty-file source's edge only
                    // for rels the overlay re-resolves (its truth is in
                    // overlay_in below); other rels keep the base edge with
                    // the source redirected into merged space.
                    let next_idx = match view {
                        Some(v) => {
                            if v.masks_base_edge(src as u32, RelType::from(&edge.rel_type)) {
                                continue;
                            }
                            match v.redirect(src as u32) {
                                Some(s) => s as usize,
                                None => continue, // source deleted on disk
                            }
                        }
                        None => src,
                    };
                    consider(MergedEdgeRef::Base(edge), next_idx);
                }
            }
            if let Some(v) = view {
                for (_, e) in v.overlay_in(curr_idx as u32) {
                    consider(MergedEdgeRef::Overlay(e), e.source as usize);
                }
            }
        }

        if matches!(direction, Direction::Down | Direction::Both) {
            // Base OUT-edges anchor mirrors in_anchor: a replaced virtual
            // node keeps its base twin's edges for rels the overlay can't
            // rebuild (masks_base_edge filters the rebuilt ones).
            let out_anchor = if curr_idx < base_len {
                Some(curr_idx)
            } else {
                view.and_then(|v| v.node(curr_idx as u32))
                    .and_then(|n| n.replaced_base)
                    .map(|b| b as usize)
            };
            if let Some(anchor) = out_anchor {
                let out_start = graph.out_offsets[anchor].to_native() as usize;
                let out_end = graph.out_offsets[anchor + 1].to_native() as usize;
                for i in out_start..out_end {
                    let edge = &graph.edges[i];
                    let target = edge.target.to_native() as usize;
                    let next_idx = match view {
                        Some(v) => {
                            if v.masks_base_edge(anchor as u32, RelType::from(&edge.rel_type)) {
                                continue;
                            }
                            match v.redirect(target as u32) {
                                Some(t) => t as usize,
                                None => continue, // target deleted on disk
                            }
                        }
                        None => target,
                    };
                    consider(MergedEdgeRef::Base(edge), next_idx);
                }
            }
            if let Some(v) = view {
                for (_, e) in v.overlay_out(curr_idx as u32) {
                    consider(MergedEdgeRef::Overlay(e), e.target as usize);
                }
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
