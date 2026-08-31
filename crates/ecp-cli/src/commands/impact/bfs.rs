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
pub(crate) struct MergedNodeMeta {
    pub(super) uid: u64,
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) file_path: String,
    pub(crate) line: u32,
}

pub(crate) fn merged_node_meta(
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

/// Whether BFS may stand on a merged-space node.
///
/// Two exclusions, both of which also decide what `run_bfs` emits: synthetic
/// nodes (`Decorates` can reach an Annotation at SYNTHETIC_FILE_IDX) have no
/// file:line to report, and test files stay out unless asked for. Virtual
/// overlay nodes come from freshly parsed source, never from synthetic
/// emission, so only the test-path check applies to them.
///
/// `test_path_cache` is keyed by base file index; overlay nodes carry their
/// own path and bypass it.
fn node_traversable(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    idx: usize,
    include_tests: bool,
    test_path_cache: &mut HashMap<usize, bool>,
) -> bool {
    if idx < graph.nodes.len() {
        let node = &graph.nodes[idx];
        if !node.has_owning_file() {
            return false;
        }
        if include_tests {
            return true;
        }
        let file_idx = node.file_idx.to_native() as usize;
        !*test_path_cache
            .entry(file_idx)
            .or_insert_with(|| is_test_path(graph.files[file_idx].path.resolve(&graph.string_pool)))
    } else {
        let vn = view
            .and_then(|v| v.node(idx as u32))
            .expect("virtual index probed without a view");
        include_tests || !is_test_path(&vn.rel_path)
    }
}

/// Whether an edge may be followed, and why not when it may not.
///
/// Shared by [`run_bfs`] and [`shortest_path`] so a path only ever runs along
/// edges the blast radius would also have walked. Two answers about the same
/// graph disagreeing on which edges are real would be worse than one answer.
pub(crate) enum EdgeVerdict {
    /// Follow it. `heuristic` marks a `RelType::is_heuristic` edge, which the
    /// caller reports in its own bucket.
    Follow { heuristic: bool },
    /// Confidence below the caller's gate.
    BelowConfidence,
    /// Heuristic edge with `include_heuristic` off.
    HeuristicSuppressed,
    /// Structural containment, or filtered out by `--relation_types`. Neither
    /// is a hidden edge worth counting: containment is never a caller, and a
    /// relation filter is the caller asking for exactly this.
    NotTraversable,
}

pub(crate) fn admit_edge(
    edge: &MergedEdge<'_>,
    min_conf: f32,
    rel_filter: &Option<Vec<String>>,
    include_heuristic: bool,
) -> EdgeVerdict {
    if edge.confidence() < min_conf {
        return EdgeVerdict::BelowConfidence;
    }
    let rel = edge.rel_type();
    // Structural containment edges (Defines, HasMethod, HasProperty, Imports)
    // describe where a symbol lives, not who calls it. Exclude from BFS so
    // File→Function Defines does not register as a caller.
    if rel.is_scope_containment() {
        return EdgeVerdict::NotTraversable;
    }
    let heuristic = rel.is_heuristic();
    if heuristic && !include_heuristic {
        return EdgeVerdict::HeuristicSuppressed;
    }
    if let Some(rels) = rel_filter.as_ref() {
        let rel_str = rel_type_to_str(rel);
        if !rels.iter().any(|r| r == rel_str) {
            return EdgeVerdict::NotTraversable;
        }
    }
    EdgeVerdict::Follow { heuristic }
}

/// The edge that reached a node during the walk.
struct Hop {
    from: usize,
    rel: &'static str,
    reason: String,
    confidence: f32,
    heuristic: bool,
}

/// One node on a resolved path, with the edge that reached it. `via` is `None`
/// on the first step, which is the start node itself.
pub(crate) struct PathStep {
    pub(crate) meta: MergedNodeMeta,
    pub(crate) via: Option<PathEdge>,
}

pub(crate) struct PathEdge {
    /// Relation type, e.g. `calls` / `extends`. The default walk follows every
    /// non-containment relation, so the reason string alone cannot say which
    /// kind of hop this was — and "A extends B" is a different fact from
    /// "A calls B".
    pub(crate) rel: &'static str,
    pub(crate) reason: String,
    pub(crate) confidence: f32,
    pub(crate) heuristic: bool,
}

/// Shortest path from any node in `starts` to any node in `goals`, along the
/// edges [`run_bfs`] would walk under the same filters.
///
/// Multi-source and multi-goal on purpose: `ecp path A B` resolves both names
/// to candidate sets, and seeding the frontier with every A candidate at depth
/// 0 costs one BFS instead of |A| x |B| of them. The returned chain names the
/// endpoints it actually used, so an overloaded name stays unambiguous in the
/// answer rather than in a flag the caller has to guess at.
///
/// Memory is bounded by the reached set, one `usize` pair plus the edge reason
/// per node — the same order as `run_bfs`'s visited set, and far below its
/// per-node `serde_json::Value`. The walk stops at the first goal popped, so
/// a near hit never pays for the full radius.
///
/// A node that is both a start and a goal yields the zero-hop path: `from` and
/// `to` resolved to the same symbol. Finding a cycle back to a start is a
/// different question and this is not it.
///
/// Returns `None` when no goal is reachable within `max_depth`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn shortest_path(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    starts: &[usize],
    goals: &HashSet<usize>,
    direction: &Direction,
    max_depth: usize,
    min_conf: f32,
    include_tests: bool,
    rel_filter: &Option<Vec<String>>,
    include_heuristic: bool,
) -> Option<Vec<PathStep>> {
    // node -> the hop that reached it; `None` marks a seed.
    let merged = MergedGraph::new(graph, view);
    let mut pred: HashMap<usize, Option<Hop>> = HashMap::new();
    let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
    let mut test_path_cache = HashMap::new();

    for &start in starts {
        if pred.insert(start, None).is_none() {
            queue.push_back((start, 0));
        }
    }

    let mut reached = None;
    while let Some((curr_idx, curr_depth)) = queue.pop_front() {
        if !node_traversable(graph, view, curr_idx, include_tests, &mut test_path_cache) {
            continue;
        }
        if goals.contains(&curr_idx) {
            reached = Some(curr_idx);
            break;
        }
        if curr_depth >= max_depth {
            continue;
        }

        let mut consider = |edge: &MergedEdge<'_>, next_idx: usize| {
            let heuristic = match admit_edge(edge, min_conf, rel_filter, include_heuristic) {
                EdgeVerdict::Follow { heuristic } => heuristic,
                _ => return,
            };
            if let std::collections::hash_map::Entry::Vacant(slot) = pred.entry(next_idx) {
                slot.insert(Some(Hop {
                    from: curr_idx,
                    rel: rel_type_to_str(edge.rel_type()),
                    reason: edge.reason(graph).to_string(),
                    confidence: edge.confidence(),
                    heuristic,
                }));
                queue.push_back((next_idx, curr_depth + 1));
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

    // Walk the hop chain back to its seed, then flip: the path is at most
    // `max_depth + 1` nodes, so materialising the display metadata here costs
    // nothing next to the walk that found them.
    let mut chain: Vec<PathStep> = Vec::new();
    let mut cursor = reached?;
    loop {
        let hop = pred.get(&cursor).expect("reached node carries a hop entry");
        let meta = merged_node_meta(graph, view, cursor);
        match hop {
            Some(h) => {
                chain.push(PathStep {
                    meta,
                    via: Some(PathEdge {
                        rel: h.rel,
                        reason: h.reason.clone(),
                        confidence: h.confidence,
                        heuristic: h.heuristic,
                    }),
                });
                cursor = h.from;
            }
            None => {
                chain.push(PathStep { meta, via: None });
                break;
            }
        }
    }
    chain.reverse();
    Some(chain)
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
    max_results: Option<usize>,
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
        if !node_traversable(graph, view, curr_idx, include_tests, &mut test_path_cache) {
            continue;
        }
        if curr_idx < base_len {
            let curr_node = &graph.nodes[curr_idx];
            let file_idx = curr_node.file_idx.to_native() as usize;
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
            let vn = view
                .and_then(|v| v.node(curr_idx as u32))
                .expect("virtual index enqueued without a view");
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

        // A caller that only wants the first N reached nodes stops the walk
        // here rather than filtering afterwards: at depth 2 a hub symbol
        // reaches six figures of nodes, and every one of them is materialised
        // as an owned `Value` above. `None` keeps the exhaustive traversal the
        // CLI needs.
        if max_results.is_some_and(|cap| det_results.len() + heur_results.len() >= cap) {
            break;
        }

        if curr_depth >= max_depth {
            continue;
        }

        // ── expansion ───────────────────────────────────────────────────
        // Shared filter chain + enqueue for base and overlay edges.
        let mut consider = |edge: &MergedEdge<'_>, next_idx: usize| {
            let is_heur = match admit_edge(edge, min_conf, rel_filter, include_heuristic) {
                EdgeVerdict::Follow { heuristic } => heuristic,
                EdgeVerdict::BelowConfidence => {
                    hidden_conf_edges += 1;
                    return;
                }
                EdgeVerdict::HeuristicSuppressed => {
                    hidden_heuristic_edges += 1;
                    return;
                }
                EdgeVerdict::NotTraversable => return,
            };
            if visited.insert(next_idx) {
                queue.push_back((
                    next_idx,
                    curr_depth + 1,
                    Some((edge.reason(graph).to_string(), edge.confidence())),
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
