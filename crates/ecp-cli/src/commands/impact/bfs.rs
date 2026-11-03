use super::Direction;
use crate::commands::format::{kind_to_str, node_kind_to_str, rel_type_to_str};
use ecp_core::contracts::raw_records::Evidence;
use ecp_core::file_category::is_test_path;
use ecp_core::file_category::Language;
use ecp_core::graph::{ArchivedFileCategory, FunctionMeta, RelType};
use ecp_core::session::{MergedEdge, MergedGraph, OverlayView};
use rustc_hash::FxHashMap;
use serde_json::{json, Value};
use std::collections::hash_map::Entry;
use std::collections::{HashSet, VecDeque};

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

/// Whether a walk may stand on a merged-space node.
///
/// Two exclusions, the same two `run_bfs` applies while it emits: synthetic
/// nodes (`Decorates` can reach an Annotation at SYNTHETIC_FILE_IDX) have no
/// file:line to report, and test nodes ([`base_node_is_test`]) stay out
/// unless asked for. Virtual overlay nodes come from freshly parsed source,
/// never from synthetic emission, so only the test check applies to them.
///
/// **This mirrors the guard `run_bfs` keeps inline, deliberately** — routing
/// `run_bfs` through it measured ~2% slower on the vscode corpus, and
/// per-query latency is this project's first priority. The method and the
/// numbers are in the `perf(path): leave run_bfs alone` commit. Change either
/// copy and run `path_walks_the_same_graph_as_impact` in
/// `tests/path_query.rs`, which holds the two together.
///
/// A test node is one in a test file or one whose `FunctionMeta` carries
/// `FLAG_TEST` ([`base_node_is_test`], [`virtual_node_is_test`]).
fn node_traversable(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    idx: usize,
    include_tests: bool,
) -> bool {
    if idx < graph.nodes.len() {
        let node = &graph.nodes[idx];
        if !node.has_owning_file() {
            return false;
        }
        include_tests || !base_node_is_test(graph, idx)
    } else {
        let view = view.expect("virtual index probed without a view");
        include_tests || !virtual_node_is_test(view, idx as u32)
    }
}

/// A base node is a test when its file is a test file or its own
/// `FunctionMeta` carries `FLAG_TEST`: a `#[cfg(test)]` function in a
/// production file, which `cypher` reports as `is_test` too. A synthetic
/// node has no file.
fn base_node_is_test(graph: &ecp_core::graph::ArchivedZeroCopyGraph, idx: usize) -> bool {
    let node = &graph.nodes[idx];
    (node.has_owning_file()
        && matches!(
            graph.files[node.file_idx.to_native() as usize].category,
            ArchivedFileCategory::Test
        ))
        || base_meta_is_test(graph, idx)
}

/// [`base_node_is_test`] or [`virtual_node_is_test`] for a merged-space
/// index: the test rule `impact` walks by, for `inspect` to filter by.
pub(crate) fn merged_node_is_test(merged: &MergedGraph<'_>, idx: u32) -> bool {
    match merged.view() {
        Some(view) if idx >= merged.base_len() => virtual_node_is_test(view, idx),
        _ => base_node_is_test(merged, idx as usize),
    }
}

/// Whether the base node's `FunctionMeta` carries `FLAG_TEST`: the dense
/// `node_flags` byte, else the sorted `function_metas` table (a graph
/// without `node_flags`).
fn base_meta_is_test(graph: &ecp_core::graph::ArchivedZeroCopyGraph, idx: usize) -> bool {
    match graph.node_flags.get(idx) {
        Some(&flags) => u16::from(flags) & FunctionMeta::FLAG_TEST != 0,
        None => graph
            .function_metas
            .binary_search_by_key(&(idx as u32), |m| m.node_idx.to_native())
            .is_ok_and(|i| {
                graph.function_metas[i].flags.to_native() & FunctionMeta::FLAG_TEST != 0
            }),
    }
}

/// [`base_node_is_test`] for an overlay node: its path, then the flags its
/// fragment paired.
fn virtual_node_is_test(view: &OverlayView, idx: u32) -> bool {
    let vn = view.node(idx).expect("virtual index probed without a node");
    is_test_path(&vn.rel_path)
        || view
            .function_meta(idx)
            .is_some_and(|meta| meta.flags & FunctionMeta::FLAG_TEST != 0)
}

/// The row key that tags a caller reached through an edge to verify.
pub(super) const REQUIRES_VERIFICATION: &str = "requires_verification";

/// Whether an edge is a lead to verify, not a resolved fact: it reads back as
/// [`Evidence::Heuristic`], a heuristic relation or a confidence below
/// [`HEURISTIC_CEILING`](ecp_core::contracts::raw_records::HEURISTIC_CEILING)
/// (0.7, which is not below itself).
fn edge_needs_verification(rel: RelType, confidence: f32) -> bool {
    Evidence::of_stored(confidence, rel) == Evidence::Heuristic
}

/// `Some((requires_verification, confidence))` when a walk may follow this edge.
///
/// Mirrors the filter chain `run_bfs` keeps inline, for the same measured
/// reason as [`node_traversable`]. A path must only ever run along edges the
/// blast radius would also have walked — two answers about the same graph
/// disagreeing on which edges are real would be worse than one answer.
///
/// The confidence rides along because the caller needs it for the step it is
/// about to record. No reason is returned for a rejection: `run_bfs` separates
/// them to fill its `hidden_edges` counters, and `shortest_path` treats all
/// four the same way.
fn admit_edge(
    edge: &MergedEdge<'_>,
    min_conf: f32,
    rel_filter: &Option<Vec<String>>,
    include_heuristic: bool,
) -> Option<(bool, f32)> {
    let confidence = edge.confidence();
    if confidence < min_conf {
        return None;
    }
    let rel = edge.rel_type();
    // Structural containment edges (Defines, HasMethod, HasProperty, Imports)
    // describe where a symbol lives, not who calls it. Exclude from BFS so
    // File→Function Defines does not register as a caller.
    if rel.is_scope_containment() {
        return None;
    }
    if rel.is_heuristic() && !include_heuristic {
        return None;
    }
    if let Some(rels) = rel_filter.as_ref() {
        let rel_str = rel_type_to_str(rel);
        if !rels.iter().any(|r| r == rel_str) {
            return None;
        }
    }
    Some((edge_needs_verification(rel, confidence), confidence))
}

/// How a walk reached a node, for choosing one among the paths of the
/// shortest depth. `verify`: some edge on the path needs verification;
/// `heuristic`: the via edge is a heuristic relation; `joined`: the node
/// joined as a part of a C# partial type, not through an edge of its own.
#[derive(Clone, Copy)]
struct Rank {
    verify: bool,
    heuristic: bool,
    confidence: f32,
    joined: bool,
}

impl Rank {
    const SEED: Rank = Rank {
        verify: false,
        heuristic: false,
        confidence: 1.0,
        joined: false,
    };

    /// Lower `(verify, heuristic)` wins, then an edge of the node's own over
    /// a part join, then the higher via confidence. A strict order, so the
    /// order of the in-edges never picks the bucket, the via edge or the tag.
    /// The own-edge step comes before confidence because a join is always
    /// 1.0: ranked by confidence alone, the part found first would keep the
    /// join over its own edge, and the edge order would pick the via edge.
    fn beats(self, other: Rank) -> bool {
        (other.verify, other.heuristic, other.joined)
            .cmp(&(self.verify, self.heuristic, self.joined))
            .then(self.confidence.total_cmp(&other.confidence))
            .is_gt()
    }

    /// The reach a part of a C# partial type takes from the part that was
    /// reached: the same tags, through `PartOf` at 1.0.
    fn joined(self) -> Rank {
        Rank {
            confidence: 1.0,
            joined: true,
            ..self
        }
    }
}

/// The edge that reached a node during the walk: the step's own payload plus
/// where it came from, and the depth and [`Rank`] of the path through it.
struct Hop<'g> {
    from: usize,
    via: PathEdge<'g>,
    depth: usize,
    rank: Rank,
}

impl Hop<'static> {
    /// The hop that joins a part of a C# partial type to the part `from`.
    fn part_of(from: usize, depth: usize, rank: Rank) -> Self {
        let part_of = rel_type_to_str(RelType::PartOf);
        Hop {
            from,
            via: PathEdge {
                rel: part_of,
                reason: part_of,
                confidence: 1.0,
                requires_verification: false,
            },
            depth,
            rank: rank.joined(),
        }
    }
}

/// Records `hop` on `idx` when the node is new, or when it was reached at the
/// same depth through a hop `hop` [beats](Rank::beats); a seed stays. Returns
/// whether it recorded `hop`. The walk records every depth-d discovery before
/// it pops any depth-d node, so a replaced node is still queued.
fn settle_hop<'g>(
    pred: &mut FxHashMap<usize, Option<Hop<'g>>>,
    queue: &mut VecDeque<(usize, usize)>,
    idx: usize,
    hop: Hop<'g>,
) -> bool {
    match pred.entry(idx) {
        Entry::Vacant(slot) => {
            queue.push_back((idx, hop.depth));
            slot.insert(Some(hop));
            true
        }
        Entry::Occupied(mut slot) => match slot.get_mut() {
            Some(seen) if seen.depth == hop.depth && hop.rank.beats(seen.rank) => {
                *seen = hop;
                true
            }
            _ => false,
        },
    }
}

/// One node on a resolved path, with the edge that reached it. `via` is `None`
/// on the first step, which is the start node itself.
pub(crate) struct PathStep<'g> {
    pub(crate) meta: MergedNodeMeta,
    pub(crate) via: Option<PathEdge<'g>>,
}

/// `reason` borrows the graph's string pool rather than owning a copy: the walk
/// discovers far more nodes than end up on the path, and a hub with a hundred
/// thousand neighbours would otherwise allocate a hundred thousand strings to
/// render at most `depth + 1` of them.
#[derive(Clone, Copy)]
pub(crate) struct PathEdge<'g> {
    /// Relation type, e.g. `calls` / `extends`. The default walk follows every
    /// non-containment relation, so the reason string alone cannot say which
    /// kind of hop this was — and "A extends B" is a different fact from
    /// "A calls B".
    pub(crate) rel: &'static str,
    pub(crate) reason: &'g str,
    pub(crate) confidence: f32,
    /// The edge reads back as [`Evidence::Heuristic`] ([`edge_needs_verification`]).
    pub(crate) requires_verification: bool,
}

/// Which edges and nodes a walk may use.
///
/// One value instead of six parameters threaded twice: a miss re-runs the walk
/// in the flipped direction, and passing the same six along by hand was a
/// function whose whole body was the re-passing.
#[derive(Clone)]
pub(crate) struct WalkOpts<'a> {
    pub(crate) direction: Direction,
    pub(crate) depth: usize,
    pub(crate) min_conf: f32,
    pub(crate) include_tests: bool,
    pub(crate) rel_filter: &'a Option<Vec<String>>,
    pub(crate) include_heuristic: bool,
}

impl<'a> WalkOpts<'a> {
    /// The same walk in the other direction, or `None` for `Both`, which has no
    /// other direction to try.
    pub(crate) fn flipped(&self) -> Option<WalkOpts<'a>> {
        let direction = match self.direction {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
            Direction::Both => return None,
        };
        Some(WalkOpts {
            direction,
            ..self.clone()
        })
    }
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
/// Among the routes of the shortest length the one with the best [`Rank`]
/// wins, as in [`run_bfs`], so a fully verified route beats a tagged one.
///
/// Returns `None` when no goal is reachable within `opts.depth`.
pub(crate) fn shortest_path<'g>(
    graph: &'g ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&'g OverlayView>,
    starts: &[usize],
    goals: &HashSet<usize>,
    opts: &WalkOpts<'_>,
) -> Option<Vec<PathStep<'g>>> {
    // node -> the hop that reached it; `None` marks a seed.
    let merged = MergedGraph::new(graph, view);
    let mut pred: FxHashMap<usize, Option<Hop<'g>>> = FxHashMap::default();
    let mut queue: VecDeque<(usize, usize)> = VecDeque::new();

    for &start in starts {
        if pred.insert(start, None).is_none() {
            queue.push_back((start, 0));
        }
    }
    // The parts of a C# partial type are one type: they join at the depth a
    // part is reached at, as `run_bfs` joins them, so `path` and `impact`
    // agree.
    for &start in starts {
        for part in joinable_parts(graph, view, &merged, start, true) {
            settle_hop(
                &mut pred,
                &mut queue,
                part as usize,
                Hop::part_of(start, 0, Rank::SEED),
            );
        }
    }

    let mut reached = None;
    while let Some((curr_idx, curr_depth)) = queue.pop_front() {
        // Both endpoints are nodes the user named, so neither is filtered;
        // only the hops between them are. `path A B` and `path B A` then
        // agree, and `run_bfs` likewise never drops its depth-0 target.
        if curr_depth > 0
            && !goals.contains(&curr_idx)
            && !node_traversable(graph, view, curr_idx, opts.include_tests)
        {
            continue;
        }
        if goals.contains(&curr_idx) {
            // Every depth-d rank is final once a depth-d node pops, so among
            // the goals at this depth the best rank wins, not the goal the
            // edge order queued first. A depth-0 goal is a seed with no hop,
            // so it falls back to itself.
            let at_depth = |g: usize| match pred.get(&g) {
                Some(Some(hop)) if hop.depth == curr_depth => Some((g, hop.rank)),
                _ => None,
            };
            reached = goals
                .iter()
                .filter_map(|&g| at_depth(g))
                .reduce(|best, c| {
                    if c.1.beats(best.1) || (!best.1.beats(c.1) && c.0 < best.0) {
                        c
                    } else {
                        best
                    }
                })
                .map_or(Some(curr_idx), |(g, _)| Some(g));
            break;
        }
        if curr_depth >= opts.depth {
            continue;
        }
        let curr_verify = pred[&curr_idx].as_ref().is_some_and(|hop| hop.rank.verify);

        let mut consider = |edge: &MergedEdge<'g>, next_idx: usize| {
            let Some((requires_verification, confidence)) =
                admit_edge(edge, opts.min_conf, opts.rel_filter, opts.include_heuristic)
            else {
                return;
            };
            let depth = curr_depth + 1;
            let rank = Rank {
                verify: curr_verify || requires_verification,
                heuristic: edge.rel_type().is_heuristic(),
                confidence,
                joined: false,
            };
            let hop = Hop {
                from: curr_idx,
                via: PathEdge {
                    rel: rel_type_to_str(edge.rel_type()),
                    reason: edge.reason(graph),
                    confidence,
                    requires_verification,
                },
                depth,
                rank,
            };
            if settle_hop(&mut pred, &mut queue, next_idx, hop) {
                for part in joinable_parts(graph, view, &merged, next_idx, opts.include_tests) {
                    settle_hop(
                        &mut pred,
                        &mut queue,
                        part as usize,
                        Hop::part_of(next_idx, depth, rank),
                    );
                }
            }
        };

        if matches!(opts.direction, Direction::Up | Direction::Both) {
            for edge in merged.in_edges(curr_idx as u32) {
                let source = edge.source as usize;
                consider(&edge, source);
            }
        }
        if matches!(opts.direction, Direction::Down | Direction::Both) {
            for edge in merged.out_edges(curr_idx as u32) {
                let target = edge.target as usize;
                consider(&edge, target);
            }
        }
    }

    // Walk the hop chain back to its seed, then flip: the path is at most
    // `max_depth + 1` nodes, so materialising the display metadata here costs
    // nothing next to the walk that found them.
    let mut chain: Vec<PathStep<'g>> = Vec::new();
    let mut cursor = reached?;
    loop {
        let hop = pred.get(&cursor).expect("reached node carries a hop entry");
        let meta = merged_node_meta(graph, view, cursor);
        match hop {
            Some(h) => {
                chain.push(PathStep {
                    meta,
                    via: Some(h.via),
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

/// The other parts of the C# `partial` type that `idx` is a part of, or none
/// for any other node. Only a C# type can have parts; the kind and path test
/// spares every other node the edge scan.
fn csharp_partial_parts(merged: &MergedGraph<'_>, idx: usize) -> Vec<u32> {
    let is_csharp_type = merged.node(idx as u32).is_some_and(|node| {
        node.kind().declares_type()
            && node
                .file_path(merged)
                .is_some_and(|f| Language::from_normalized_path(f) == Language::CSharp)
    });
    if is_csharp_type {
        merged.partial_parts(idx as u32)
    } else {
        Vec::new()
    }
}

/// The parts of a C# partial type that join a walk at the depth the part
/// `idx` was reached at: every part but `idx`, and none when the walk may not
/// stand on `idx` ([`node_traversable`]), as no part joins through a node the
/// walk skips.
fn joinable_parts(
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&OverlayView>,
    merged: &MergedGraph<'_>,
    idx: usize,
    include_tests: bool,
) -> Vec<u32> {
    let mut parts = csharp_partial_parts(merged, idx);
    if !parts.is_empty() && !node_traversable(graph, view, idx, include_tests) {
        parts.clear();
    }
    parts.retain(|&part| part as usize != idx);
    parts
}

/// Constructors an upstream walk from a Class / Struct start treats as the
/// start itself: a construction's `Calls` edge lands on the type's sole
/// constructor when it has one, else on the type, so the instantiators are
/// the callers of both. Empty for any other start and for a downstream-only
/// walk.
///
/// The constructors of every part of a C# `partial` type count: a part may
/// declare a constructor in another file than the start.
fn constructor_seeds(merged: &MergedGraph<'_>, start_idx: u32, direction: &Direction) -> Vec<u32> {
    if *direction == Direction::Down {
        return Vec::new();
    }
    let parts = merged.partial_parts(start_idx);
    if parts.is_empty() {
        return merged.constructors_of(start_idx);
    }
    parts
        .into_iter()
        .flat_map(|part| merged.constructors_of(part))
        .collect()
}

/// Core BFS over the merged graph (base CSR + optional overlay view) from
/// `start_idx`, which is a MERGED-space index: `< graph.nodes.len()` = base
/// node, above = overlay virtual node.
///
/// Returns `(det_results, heur_results, hidden_conf_edges, hidden_heuristic_edges,
/// hidden_test_callers)`.
/// The start node appears at depth 0 in `det_results`. An upstream walk from
/// a Class / Struct also expands its [`constructor_seeds`] at depth 0; the
/// constructors themselves are never emitted.
///
/// - `det_results`: nodes reached exclusively via deterministic edges.
/// - `heur_results`: nodes reached via a heuristic edge (only populated when
///   `include_heuristic` is true; kept in a separate vec so callers render them
///   in a distinct output section per the T-H1 spec).
/// - `hidden_conf_edges`: edges dropped because confidence < `min_conf`.
/// - `hidden_heuristic_edges`: heuristic edges skipped when `include_heuristic`
///   is false. These are the structural signal surfaced as
///   `hidden_heuristic_edges: N` in the output payload.
/// - `hidden_test_callers`: test nodes reached but dropped because
///   `include_tests` is false. Each node counts once (the visited set). The
///   depth-0 target is never filtered, so it is never counted.
///
/// Every emitted node carries `test: bool`, so a caller list that mixes
/// production and test callers stays separable, and
/// [`REQUIRES_VERIFICATION`]: some edge on its path needs verification. Among
/// the paths of the shortest depth the one with the best [`Rank`] wins, with
/// its via edge, tag and bucket: fewest tags first, then the higher via
/// confidence, whichever in-edge the walk met first.
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
pub(super) fn run_bfs<'g>(
    graph: &'g ecp_core::graph::ArchivedZeroCopyGraph,
    view: Option<&'g OverlayView>,
    start_idx: usize,
    direction: &Direction,
    max_depth: usize,
    min_conf: f32,
    include_tests: bool,
    rel_filter: &Option<Vec<String>>,
    include_heuristic: bool,
    max_results: Option<usize>,
) -> (Vec<Value>, Vec<Value>, u64, u64, u64) {
    // How the walk reached a node, kept in the visited map and read when the
    // node pops. The via reason is borrowed from the string pool: a hub
    // enqueues six figures of nodes at depth 2, and an owned String per node
    // was the largest allocation here. `rank.heuristic` picks the output
    // bucket; `rank.verify` is the separate tag: some edge on the path is
    // heuristic or below `HEURISTIC_CEILING`.
    #[derive(Clone, Copy)]
    struct Reach<'a> {
        depth: usize,
        via: Option<&'a str>,
        rank: Rank,
    }
    const START: Reach<'static> = Reach {
        depth: 0,
        via: None,
        rank: Rank::SEED,
    };

    /// Records a reach of `idx` at `depth` when the node is new, or when it
    /// was reached at this same depth through a path `rank`
    /// [beats](Rank::beats). Returns whether it recorded one. The walk
    /// records every depth-d discovery before it pops any depth-d node, so a
    /// replaced node is still queued. `via` runs only when it records.
    fn settle<'a>(
        visited: &mut FxHashMap<usize, Reach<'a>>,
        queue: &mut VecDeque<usize>,
        idx: usize,
        depth: usize,
        rank: Rank,
        via: impl FnOnce() -> &'a str,
    ) -> bool {
        match visited.entry(idx) {
            Entry::Vacant(slot) => {
                slot.insert(Reach {
                    depth,
                    via: Some(via()),
                    rank,
                });
                queue.push_back(idx);
                true
            }
            Entry::Occupied(mut slot) => {
                let seen = slot.get_mut();
                let wins = seen.depth == depth && rank.beats(seen.rank);
                if wins {
                    *seen = Reach {
                        depth,
                        via: Some(via()),
                        rank,
                    };
                }
                wins
            }
        }
    }
    let part_of = || -> &'g str { rel_type_to_str(RelType::PartOf) };

    let merged = MergedGraph::new(graph, view);
    let base_len = graph.nodes.len();
    let mut visited: FxHashMap<usize, Reach<'g>> = FxHashMap::default();
    let mut queue: VecDeque<usize> = VecDeque::new();
    let mut det_results: Vec<Value> = Vec::new();
    let mut heur_results: Vec<Value> = Vec::new();
    let mut hidden_conf_edges: u64 = 0;
    let mut hidden_heuristic_edges: u64 = 0;
    let mut hidden_test_callers: u64 = 0;

    queue.push_back(start_idx);
    visited.insert(start_idx, START);
    // The parts of a C# partial type are one type: every part joins the walk
    // at the depth a part is reached at, in any direction and under any
    // relation filter, so a walk from any part reaches the same nodes.
    for part in joinable_parts(graph, view, &merged, start_idx, true) {
        settle(
            &mut visited,
            &mut queue,
            part as usize,
            0,
            Rank::SEED.joined(),
            part_of,
        );
    }
    let ctor_seeds = constructor_seeds(&merged, start_idx as u32, direction);
    visited.extend(ctor_seeds.iter().map(|&c| (c as usize, START)));

    while let Some(curr_idx) = queue.pop_front() {
        let Reach {
            depth: curr_depth,
            via,
            rank: curr_rank,
        } = visited[&curr_idx];
        // ── node emission (merged space: < base_len = base, else virtual) ──
        let (uid, name, kind_str, file_path, line, is_test): (
            u64,
            String,
            &'static str,
            String,
            u32,
            bool,
        );
        if curr_idx < base_len {
            let curr_node = &graph.nodes[curr_idx];
            // BFS via `Decorates` edges can reach synthetic Annotation nodes
            // (SYNTHETIC_FILE_IDX); they have no file:line to report.
            if !curr_node.has_owning_file() {
                continue;
            }
            let file_idx = curr_node.file_idx.to_native() as usize;
            is_test = base_node_is_test(graph, curr_idx);
            // Depth 0 is the target itself, never a caller to filter.
            if is_test && !include_tests && curr_depth > 0 {
                hidden_test_callers += 1;
                continue;
            }
            uid = curr_node.uid.to_native();
            name = curr_node.name.resolve(&graph.string_pool).to_string();
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
            // A virtual Annotation the replay made for a decorator nothing
            // declares has no file:line, as the base branch's.
            if !vn.has_owning_file() {
                continue;
            }
            is_test =
                virtual_node_is_test(view.expect("a virtual node has a view"), curr_idx as u32);
            if is_test && !include_tests && curr_depth > 0 {
                hidden_test_callers += 1;
                continue;
            }
            uid = vn.uid;
            name = vn.name.clone();
            kind_str = node_kind_to_str(&vn.kind);
            file_path = vn.rel_path.to_string();
            line = vn.start_line;
        }

        let (via_reason, via_confidence) = (via.unwrap_or(""), curr_rank.confidence);
        let curr = curr_idx as u32;
        let entry = json!({
            "uid": uid.to_string(),
            "name": name,
            "ownerClass": merged.node(curr).and_then(|n| n.owner_class(&merged)),
            "lexicalParent": merged.display_owner(curr),
            "kind": kind_str,
            "filePath": file_path,
            "line": line,
            "depth": curr_depth,
            "viaReason": via_reason,
            "viaConfidence": via_confidence,
            "test": is_test,
            REQUIRES_VERIFICATION: curr_rank.verify,
        });
        if curr_rank.heuristic {
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
        let mut consider = |edge: &MergedEdge<'g>, next_idx: usize| {
            let edge_conf = edge.confidence();
            if edge_conf < min_conf {
                hidden_conf_edges += 1;
                return;
            }
            let rel = edge.rel_type();
            // Structural containment edges (Defines, HasMethod, HasProperty,
            // Imports) describe where a symbol lives, not who calls it.
            // Exclude from BFS so File→Function Defines does not register
            // as a caller. A `PartOf` edge joins the parts of one type at
            // one depth above, not as a step.
            if rel.is_scope_containment() || rel == RelType::PartOf {
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
            let depth = curr_depth + 1;
            let rank = Rank {
                verify: curr_rank.verify || edge_needs_verification(rel, edge_conf),
                heuristic: is_heur,
                confidence: edge_conf,
                joined: false,
            };
            // A part reached at depth d settles before any depth-d node pops,
            // so a part joined through a tagged edge is still replaced when
            // its sibling part arrives through a verified one.
            if settle(&mut visited, &mut queue, next_idx, depth, rank, || {
                edge.reason(graph)
            }) {
                for part in joinable_parts(graph, view, &merged, next_idx, include_tests) {
                    settle(
                        &mut visited,
                        &mut queue,
                        part as usize,
                        depth,
                        rank.joined(),
                        part_of,
                    );
                }
            }
        };

        if matches!(direction, Direction::Up | Direction::Both) {
            for edge in merged.in_edges(curr_idx as u32) {
                let source = edge.source as usize;
                consider(&edge, source);
            }
            if curr_depth == 0 {
                for edge in ctor_seeds.iter().flat_map(|&c| merged.in_edges(c)) {
                    let source = edge.source as usize;
                    consider(&edge, source);
                }
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
        hidden_test_callers,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind};
    use ecp_core::graph_fixture::GraphFixture;

    /// nodes: 0 Reader (Reader.cs, the representative) · 1 Reader
    /// (Reader.Async.cs) · 2 TextReader · 3 Read · 4 ReadAsync · 5 caller
    /// edges: 1 →PartOf→ 0, TextReader →Extends→ 0, HasMethod 0→Read and
    /// 1→ReadAsync, caller →Calls→ ReadAsync.
    fn partial_graph() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        let main = fx.node(NodeKind::Class, "Lib/Reader.cs", "Reader");
        let async_part = fx.node(NodeKind::Class, "Lib/Reader.Async.cs", "Reader");
        let text_reader = fx.node(NodeKind::Class, "Lib/TextReader.cs", "TextReader");
        let read = fx.method("Lib/Reader.cs", "Reader", "Read");
        let read_async = fx.method("Lib/Reader.Async.cs", "Reader", "ReadAsync");
        let caller = fx.func("Lib/Use.cs", "caller");
        fx.edge(async_part, main, RelType::PartOf);
        fx.edge(text_reader, main, RelType::Extends);
        fx.edge(main, read, RelType::HasMethod);
        fx.edge(async_part, read_async, RelType::HasMethod);
        fx.edge(caller, read_async, RelType::Calls);
        fx.into_bytes()
    }

    /// `(name, file, depth)` of every node an upstream walk from `start`
    /// reaches, sorted.
    fn upstream(graph: &ArchivedZeroCopyGraph, start: usize) -> Vec<(String, String, u64)> {
        let (det, _, _, _, _) = run_bfs(
            graph,
            None,
            start,
            &Direction::Up,
            3,
            0.0,
            true,
            &None,
            false,
            None,
        );
        let mut out: Vec<(String, String, u64)> = det
            .iter()
            .map(|e| {
                (
                    e["name"].as_str().unwrap().to_string(),
                    e["filePath"].as_str().unwrap().to_string(),
                    e["depth"].as_u64().unwrap(),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// Stands for: a walk that starts at any part of a C# partial type
    /// reaches the same nodes at the same depths: both parts at depth 0 and
    /// the subclass at depth 1. Before the fix a walk from the async part
    /// reached the representative as a depth-1 `PartOf` caller and the
    /// subclass not at all within the parts' own depth.
    #[test]
    fn test_run_bfs_partial_type_any_part_walks_same_type() {
        let bytes = partial_graph();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let expected = vec![
            ("Reader".to_string(), "Lib/Reader.Async.cs".to_string(), 0),
            ("Reader".to_string(), "Lib/Reader.cs".to_string(), 0),
            ("TextReader".to_string(), "Lib/TextReader.cs".to_string(), 1),
        ];
        assert_eq!(upstream(graph, 0), expected);
        assert_eq!(upstream(graph, 1), expected);
    }

    /// Stands for: a node that is no part of a partial type walks as
    /// before; a method's caller is one step away and no type joins.
    #[test]
    fn test_run_bfs_non_partial_start_unchanged() {
        let bytes = partial_graph();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(
            upstream(graph, 4),
            [
                (
                    "ReadAsync".to_string(),
                    "Lib/Reader.Async.cs".to_string(),
                    0
                ),
                ("caller".to_string(), "Lib/Use.cs".to_string(), 1),
            ]
        );
    }

    /// nodes: 0 target · 1 exact (0.7) · 2 below (0.69) · 3 full (1.0) ·
    /// 4 behind (1.0 into `below`) · 5 mirror (`MirrorsField`, 0.9)
    fn confidence_graph() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        let target = fx.func("src/t.rs", "target");
        let exact = fx.func("src/a.rs", "exact");
        let below = fx.func("src/b.rs", "below");
        let full = fx.func("src/c.rs", "full");
        let behind = fx.func("src/d.rs", "behind");
        let mirror = fx.func("src/e.rs", "mirror");
        fx.edge_with(exact, target, RelType::Calls, 0.7, "call");
        fx.edge_with(below, target, RelType::Calls, 0.69, "call");
        fx.edge_with(full, target, RelType::Calls, 1.0, "call");
        fx.edge_with(behind, below, RelType::Calls, 1.0, "call");
        fx.edge_with(mirror, target, RelType::MirrorsField, 0.9, "schema-mirror");
        fx.into_bytes()
    }

    type Rows = Vec<(String, bool)>;

    /// `(name, requires_verification)` of every row `run_bfs` returns, sorted:
    /// the deterministic rows, then the heuristic rows.
    fn verification(
        graph: &ArchivedZeroCopyGraph,
        min_conf: f32,
        include_heuristic: bool,
    ) -> (Rows, Rows) {
        let (det, heur, _, _, _) = run_bfs(
            graph,
            None,
            0,
            &Direction::Up,
            3,
            min_conf,
            true,
            &None,
            include_heuristic,
            None,
        );
        let rows = |entries: &[Value]| {
            let mut out: Vec<(String, bool)> = entries
                .iter()
                .map(|e| {
                    (
                        e["name"].as_str().unwrap().to_string(),
                        e[REQUIRES_VERIFICATION]
                            .as_bool()
                            .expect("every row carries requires_verification"),
                    )
                })
                .collect();
            out.sort();
            out
        };
        (rows(&det), rows(&heur))
    }

    /// Stands for: a sub-0.7 `Calls` caller is tagged in place, and so is a
    /// caller behind it; 0.7 exactly and 1.0 stay untagged. Before the fix no
    /// row had the key, so `expect` panicked.
    #[test]
    fn test_run_bfs_sub_global_confidence_edges_tag_rows_in_place() {
        let bytes = confidence_graph();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let (det, heur) = verification(graph, 0.0, false);
        assert_eq!(
            det,
            [
                ("behind".to_string(), true),
                ("below".to_string(), true),
                ("exact".to_string(), false),
                ("full".to_string(), false),
                ("target".to_string(), false),
            ]
        );
        assert!(heur.is_empty(), "heuristic rows need the include flag");
    }

    /// Stands for: a heuristic relation keeps its bucket and is tagged;
    /// `--no-heuristic` hides it as before.
    #[test]
    fn test_run_bfs_heuristic_row_stays_in_heuristic_bucket_tagged() {
        let bytes = confidence_graph();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let (det, heur) = verification(graph, 0.0, true);
        assert_eq!(heur, [("mirror".to_string(), true)]);
        assert!(det.iter().all(|(name, _)| name != "mirror"));
    }

    /// Stands for: a `--min-confidence` gate leaves only untagged rows here.
    #[test]
    fn test_run_bfs_min_confidence_gate_drops_tagged_rows() {
        let bytes = confidence_graph();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let (det, _) = verification(graph, 0.8, false);
        assert_eq!(
            det,
            [("full".to_string(), false), ("target".to_string(), false)]
        );
    }

    type TaggedRows = Vec<(String, bool, f64)>;

    /// `(name, requires_verification, viaConfidence)` of every row of an
    /// upstream walk from node 0, sorted, deterministic rows then heuristic.
    fn tagged_rows(bytes: &[u8]) -> (TaggedRows, TaggedRows) {
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap();
        let (det, heur, _, _, _) = run_bfs(
            graph,
            None,
            0,
            &Direction::Up,
            3,
            0.0,
            true,
            &None,
            true,
            None,
        );
        let rows = |entries: &[Value]| {
            let mut out: Vec<(String, bool, f64)> = entries
                .iter()
                .map(|e| {
                    (
                        e["name"].as_str().unwrap().to_string(),
                        e[REQUIRES_VERIFICATION].as_bool().unwrap(),
                        e["viaConfidence"].as_f64().unwrap(),
                    )
                })
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        };
        (rows(&det), rows(&heur))
    }

    /// Stands for: `t` reaches `target` at depth 2 through `a` (all 1.0) and
    /// through `b` (0.55 into `target`). The verified path wins in either
    /// in-edge order. Before the fix the first-popped parent decided, so with
    /// `b` created (and popped) first `t` came back tagged.
    #[test]
    fn test_run_bfs_same_depth_verified_parent_untags_either_order() {
        for b_first in [false, true] {
            let mut fx = GraphFixture::new();
            let target = fx.func("src/t.rs", "target");
            let (a, b) = if b_first {
                let b = fx.func("src/b.rs", "b");
                (fx.func("src/a.rs", "a"), b)
            } else {
                let a = fx.func("src/a.rs", "a");
                (a, fx.func("src/b.rs", "b"))
            };
            let t = fx.func("src/caller.rs", "t");
            fx.edge_with(a, target, RelType::Calls, 1.0, "call");
            fx.edge_with(b, target, RelType::Calls, 0.55, "call");
            fx.edge_with(t, a, RelType::Calls, 1.0, "call");
            fx.edge_with(t, b, RelType::Calls, 1.0, "call");
            let (det, _) = tagged_rows(&fx.into_bytes());
            assert_eq!(
                det,
                [
                    ("a".to_string(), false, 1.0),
                    ("b".to_string(), true, f64::from(0.55f32)),
                    ("t".to_string(), false, 1.0),
                    ("target".to_string(), false, 1.0),
                ],
                "b_first={b_first}"
            );
        }
    }

    /// Stands for: one caller with a 0.55 and a 1.0 call site of `target`,
    /// in either edge order, is an untagged row whose via edge is the 1.0
    /// one; a caller reached by a heuristic relation and by a resolved call
    /// sits in the deterministic bucket, untagged. Before the fix the first
    /// edge decided the tag, the via confidence and the bucket.
    #[test]
    fn test_run_bfs_parallel_edges_verified_edge_wins_tag_via_and_bucket() {
        for sure_first in [false, true] {
            let mut fx = GraphFixture::new();
            let target = fx.func("src/t.rs", "target");
            let sites = fx.func("src/a.rs", "sites");
            let mirror = fx.func("src/m.rs", "mirror");
            let edges = [
                (sites, RelType::Calls, 0.55),
                (mirror, RelType::MirrorsField, 0.9),
            ];
            for (source, rel, conf) in edges {
                if sure_first {
                    fx.edge_with(source, target, RelType::Calls, 1.0, "call");
                }
                fx.edge_with(source, target, rel, conf, "guess");
                if !sure_first {
                    fx.edge_with(source, target, RelType::Calls, 1.0, "call");
                }
            }
            let (det, heur) = tagged_rows(&fx.into_bytes());
            assert_eq!(
                det,
                [
                    ("mirror".to_string(), false, 1.0),
                    ("sites".to_string(), false, 1.0),
                    ("target".to_string(), false, 1.0),
                ],
                "sure_first={sure_first}"
            );
            assert!(heur.is_empty(), "sure_first={sure_first}: {heur:?}");
        }
    }

    type ReachRows = Vec<(String, bool, f64, String)>;

    /// `(filePath, requires_verification, viaConfidence, viaReason)` of every
    /// row of an upstream walk from node 0, sorted by file: deterministic rows
    /// then heuristic rows. Keyed by file because the parts of one partial
    /// type share a name.
    fn reach_rows(bytes: &[u8]) -> (ReachRows, ReachRows) {
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap();
        let (det, heur, _, _, _) = run_bfs(
            graph,
            None,
            0,
            &Direction::Up,
            3,
            0.0,
            true,
            &None,
            true,
            None,
        );
        let rows = |entries: &[Value]| {
            let mut out: ReachRows = entries
                .iter()
                .map(|e| {
                    (
                        e["filePath"].as_str().unwrap().to_string(),
                        e[REQUIRES_VERIFICATION].as_bool().unwrap(),
                        e["viaConfidence"].as_f64().unwrap(),
                        e["viaReason"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        };
        (rows(&det), rows(&heur))
    }

    /// Stands for: parts A and B of one C# partial type both reach `T` at
    /// depth 1, A through a 0.55 edge and B through a 1.0 edge, so the type
    /// is reached verified: A and `t`, the caller of A, come back untagged in
    /// every node and edge order. On HEAD the pop-time join wrote only vacant
    /// parts, so A kept its 0.55 tag and `t` inherited it.
    #[test]
    fn test_run_bfs_partial_parts_verified_sibling_untags_part_and_caller_any_order() {
        for (a_node_first, a_edge_first) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let mut fx = GraphFixture::new();
            let target = fx.func("Lib/T.cs", "T");
            let (a, b) = if a_node_first {
                let a = fx.node(NodeKind::Class, "Lib/P.cs", "P");
                (a, fx.node(NodeKind::Class, "Lib/P.B.cs", "P"))
            } else {
                let b = fx.node(NodeKind::Class, "Lib/P.B.cs", "P");
                (fx.node(NodeKind::Class, "Lib/P.cs", "P"), b)
            };
            let t = fx.func("Lib/Use.cs", "t");
            fx.edge(b, a, RelType::PartOf);
            let edges = [(a, 0.55, "guess"), (b, 1.0, "call")];
            let ordered = if a_edge_first {
                edges
            } else {
                [edges[1], edges[0]]
            };
            for (source, conf, reason) in ordered {
                fx.edge_with(source, target, RelType::Calls, conf, reason);
            }
            fx.edge_with(t, a, RelType::Calls, 1.0, "call");
            let (det, heur) = reach_rows(&fx.into_bytes());
            let part_of = rel_type_to_str(RelType::PartOf).to_string();
            assert_eq!(
                det,
                [
                    ("Lib/P.B.cs".to_string(), false, 1.0, "call".to_string()),
                    ("Lib/P.cs".to_string(), false, 1.0, part_of),
                    ("Lib/T.cs".to_string(), false, 1.0, String::new()),
                    ("Lib/Use.cs".to_string(), false, 1.0, "call".to_string()),
                ],
                "a_node_first={a_node_first} a_edge_first={a_edge_first}"
            );
            assert!(heur.is_empty(), "{heur:?}");
        }
    }

    /// Stands for: parts A and B of one C# partial type each reach `T`
    /// verified, A at 1.0 and B at 0.8. With equal tags a part's own edge
    /// outranks a join, so each part keeps its own via edge in either order.
    /// Ranked by confidence before the own-edge step, the 1.0 join would
    /// replace B's 0.8 edge in one order and not the other.
    #[test]
    fn test_run_bfs_partial_parts_equal_tags_keep_own_via_edge_any_order() {
        for a_first in [false, true] {
            let mut fx = GraphFixture::new();
            let target = fx.func("Lib/T.cs", "T");
            let (a, b) = if a_first {
                let a = fx.node(NodeKind::Class, "Lib/P.cs", "P");
                (a, fx.node(NodeKind::Class, "Lib/P.B.cs", "P"))
            } else {
                let b = fx.node(NodeKind::Class, "Lib/P.B.cs", "P");
                (fx.node(NodeKind::Class, "Lib/P.cs", "P"), b)
            };
            fx.edge(b, a, RelType::PartOf);
            if a_first {
                fx.edge_with(a, target, RelType::Calls, 1.0, "a-call");
                fx.edge_with(b, target, RelType::Calls, 0.8, "b-call");
            } else {
                fx.edge_with(b, target, RelType::Calls, 0.8, "b-call");
                fx.edge_with(a, target, RelType::Calls, 1.0, "a-call");
            }
            let (det, _) = reach_rows(&fx.into_bytes());
            assert_eq!(
                det,
                [
                    (
                        "Lib/P.B.cs".to_string(),
                        false,
                        f64::from(0.8f32),
                        "b-call".to_string()
                    ),
                    ("Lib/P.cs".to_string(), false, 1.0, "a-call".to_string()),
                    ("Lib/T.cs".to_string(), false, 1.0, String::new()),
                ],
                "a_first={a_first}"
            );
        }
    }

    /// Stands for: `w` reaches `target` through a `MirrorsField` 0.9 edge and
    /// a `Calls` 0.5 edge. Both need verification; the deterministic one ranks
    /// first, so `w` sits in the deterministic bucket, tagged, via 0.5, in
    /// either edge order. On HEAD only a fully verified edge replaced a seen
    /// one, so with the mirror edge first `w` stayed in the heuristic bucket.
    #[test]
    fn test_run_bfs_tagged_call_beats_heuristic_edge_either_order() {
        for mirror_first in [false, true] {
            let mut fx = GraphFixture::new();
            let target = fx.func("src/t.rs", "target");
            let w = fx.func("src/w.rs", "w");
            let edges = [
                (RelType::MirrorsField, 0.9, "schema-mirror"),
                (RelType::Calls, 0.5, "call"),
            ];
            let ordered = if mirror_first {
                edges
            } else {
                [edges[1], edges[0]]
            };
            for (rel, conf, reason) in ordered {
                fx.edge_with(w, target, rel, conf, reason);
            }
            let (det, heur) = reach_rows(&fx.into_bytes());
            assert_eq!(
                det,
                [
                    ("src/t.rs".to_string(), false, 1.0, String::new()),
                    ("src/w.rs".to_string(), true, 0.5, "call".to_string()),
                ],
                "mirror_first={mirror_first}"
            );
            assert!(heur.is_empty(), "mirror_first={mirror_first}: {heur:?}");
        }
    }

    /// Stands for: two verified `Calls` edges from `w` into `target`, 0.8 and
    /// 0.9: the higher confidence is the via edge in either order. On HEAD the
    /// first edge met stayed.
    #[test]
    fn test_run_bfs_parallel_verified_edges_higher_confidence_wins_either_order() {
        for low_first in [false, true] {
            let mut fx = GraphFixture::new();
            let target = fx.func("src/t.rs", "target");
            let w = fx.func("src/w.rs", "w");
            let edges = [(0.8, "low"), (0.9, "high")];
            let ordered = if low_first {
                edges
            } else {
                [edges[1], edges[0]]
            };
            for (conf, reason) in ordered {
                fx.edge_with(w, target, RelType::Calls, conf, reason);
            }
            let (det, _) = reach_rows(&fx.into_bytes());
            assert_eq!(
                det[1],
                (
                    "src/w.rs".to_string(),
                    false,
                    f64::from(0.9f32),
                    "high".to_string()
                ),
                "low_first={low_first}"
            );
        }
    }

    /// Stands for: `ecp path S Foo` seeds two goals (the type and its
    /// constructor). `S→A→ctor` has a 0.55 hop, `S→B→Foo` is fully verified,
    /// both two hops. The verified route wins in either edge order and node
    /// order. On HEAD the goal queued first won, so with `S→A` first the
    /// answer was the tagged route to the constructor.
    #[test]
    fn test_shortest_path_same_depth_goals_best_rank_wins_any_order() {
        for (a_first, ctor_first) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut fx = GraphFixture::new();
            let s = fx.func("src/s.rs", "s");
            let (ctor, foo) = if ctor_first {
                let c = fx.func("src/foo.rs", "ctor");
                (c, fx.func("src/foo.rs", "Foo"))
            } else {
                let f = fx.func("src/foo.rs", "Foo");
                (fx.func("src/foo.rs", "ctor"), f)
            };
            let a = fx.func("src/a.rs", "a");
            let b = fx.func("src/b.rs", "b");
            let firsts = if a_first { [a, b] } else { [b, a] };
            for mid in firsts {
                fx.edge_with(s, mid, RelType::Calls, 1.0, "call");
            }
            fx.edge_with(a, ctor, RelType::Calls, 0.55, "guess");
            fx.edge_with(b, foo, RelType::Calls, 1.0, "call");
            let bytes = fx.into_bytes();
            let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
            let opts = WalkOpts {
                direction: Direction::Down,
                depth: 4,
                min_conf: 0.0,
                include_tests: true,
                rel_filter: &None,
                include_heuristic: false,
            };
            let goals = HashSet::from([foo as usize, ctor as usize]);
            let steps = shortest_path(graph, None, &[s as usize], &goals, &opts).unwrap();
            let route: Vec<_> = steps.iter().map(|st| st.meta.name.to_string()).collect();
            let tagged = steps
                .iter()
                .any(|st| st.via.is_some_and(|v| v.requires_verification));
            assert_eq!(
                (route, tagged),
                (vec!["s".into(), "b".into(), "Foo".into()], false),
                "a_first={a_first} ctor_first={ctor_first}"
            );
        }
    }
}
