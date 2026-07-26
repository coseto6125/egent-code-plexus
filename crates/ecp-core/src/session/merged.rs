//! Merged-space traversal: the base graph and its L1 overlay, behind one
//! interface.
//!
//! [`OverlayView`] describes *what* the overlay changes; applying it to a
//! traversal is a three-step protocol — drop base edges the overlay rebuilds,
//! redirect surviving endpoints into merged space, drop suppressed ones, then
//! add the overlay's own adjacency. Every traversal that skipped a step
//! answered from a graph nobody edited.
//!
//! [`MergedGraph`] performs those steps so callers iterate edges and read
//! nodes without knowing an overlay exists. With `view: None` — the clean-tree
//! case — every method degenerates to a plain walk of the archived graph.
//!
//! Merged space: index `< base_len` is a base node, `>= base_len` is a virtual
//! (dirty-file) node. Edge index `< graph.edges.len()` is a base edge, above
//! is an overlay edge, so an edge index stays addressable across the merge.
//!
//! `#[inline]` throughout: this is the per-edge inner loop of `impact` and
//! cypher, and `run_bfs` calls it from the other crate. Only `release-dist`
//! builds with LTO, so without the hint the ordinary `--release` build the
//! benchmarks use would pay a real cross-crate call per edge.
//!
//! Both endpoints of a base edge are redirected even though an adjacency
//! caller reads only the far one: it costs one `FxHashMap` lookup on a dirty
//! tree, and it is what lets `all_edges` — which has no anchor — hand back an
//! edge whose two ends are both meaningful.

use crate::graph::{ArchivedNode, ArchivedZeroCopyGraph, NodeKind, RelType};
use crate::session::view::{OverlayView, ViewEdge, ViewNode};

/// Base graph plus optional overlay, as one `Copy` bundle. Derefs to the base
/// graph so plain `graph.nodes` / `graph.files` reads stay unchanged.
#[derive(Clone, Copy)]
pub struct MergedGraph<'a> {
    graph: &'a ArchivedZeroCopyGraph,
    view: Option<&'a OverlayView>,
}

impl std::ops::Deref for MergedGraph<'_> {
    type Target = ArchivedZeroCopyGraph;
    fn deref(&self) -> &Self::Target {
        self.graph
    }
}

/// One merged-space node: an archived base node or an overlay virtual node.
pub enum MergedNode<'a> {
    Base(&'a ArchivedNode),
    Virtual(&'a ViewNode),
}

/// One merged-space edge. `source` and `target` are already redirected, so a
/// caller never sees a base index the overlay has moved.
pub struct MergedEdge<'a> {
    /// Merged edge index: base edges keep their position in `graph.edges`,
    /// overlay edges sit at `graph.edges.len() + i`.
    pub idx: u32,
    pub source: u32,
    pub target: u32,
    origin: EdgeOrigin<'a>,
}

enum EdgeOrigin<'a> {
    Base(&'a crate::graph::ArchivedEdge),
    Overlay(&'a ViewEdge),
}

impl<'a> MergedEdge<'a> {
    #[inline]
    pub fn rel_type(&self) -> RelType {
        match self.origin {
            EdgeOrigin::Base(e) => RelType::from(&e.rel_type),
            EdgeOrigin::Overlay(e) => e.rel_type,
        }
    }

    #[inline]
    pub fn confidence(&self) -> f32 {
        match self.origin {
            EdgeOrigin::Base(e) => e.confidence.to_native(),
            EdgeOrigin::Overlay(e) => e.confidence,
        }
    }

    #[inline]
    pub fn is_overlay(&self) -> bool {
        matches!(self.origin, EdgeOrigin::Overlay(_))
    }

    /// Why the edge exists. Overlay edges carry a static marker so a consumer
    /// can tell a caller comes from an uncommitted edit.
    pub fn reason(&self, graph: &'a ArchivedZeroCopyGraph) -> &'a str {
        match self.origin {
            EdgeOrigin::Base(e) => e.reason.resolve(&graph.string_pool),
            EdgeOrigin::Overlay(_) => OVERLAY_EDGE_REASON,
        }
    }
}

/// `viaReason` for an edge the overlay resolved from an uncommitted edit.
pub const OVERLAY_EDGE_REASON: &str = "l1-overlay";

impl<'a> MergedGraph<'a> {
    pub fn new(graph: &'a ArchivedZeroCopyGraph, view: Option<&'a OverlayView>) -> Self {
        Self { graph, view }
    }

    pub fn view(&self) -> Option<&'a OverlayView> {
        self.view
    }

    pub fn base_len(&self) -> u32 {
        self.graph.nodes.len() as u32
    }

    /// Number of merged-space node indices: base nodes plus virtual ones.
    pub fn node_count(&self) -> u32 {
        self.base_len() + self.view.map_or(0, |v| v.virtual_nodes().len() as u32)
    }

    /// Merged-space node fetch. `None` for an out-of-range virtual index.
    #[inline]
    pub fn node(&self, idx: u32) -> Option<MergedNode<'a>> {
        if idx < self.base_len() {
            Some(MergedNode::Base(&self.graph.nodes[idx as usize]))
        } else {
            self.view.and_then(|v| v.node(idx)).map(MergedNode::Virtual)
        }
    }

    /// Scan visibility of a BASE index: false when the view suppressed it
    /// (deleted on disk) or replaced it (its virtual twin scans instead).
    #[inline]
    pub fn base_visible(&self, idx: u32) -> bool {
        match self.view {
            Some(v) => v.redirect(idx) == Some(idx),
            None => true,
        }
    }

    /// Overlay edge behind a merged edge index, if that index is one.
    pub fn overlay_edge(&self, merged_edge_idx: u32) -> Option<&'a ViewEdge> {
        let base = self.graph.edges.len() as u32;
        merged_edge_idx
            .checked_sub(base)
            .and_then(|i| self.view.and_then(|v| v.edge(i)))
    }

    /// Base index whose CSR rows carry `idx`'s edges: the node itself when
    /// base, its uid-identical base twin when the overlay replaced it, and
    /// `None` for a brand-new symbol no base edge can reach.
    ///
    /// Callers are expected to have established that `idx` is visible —
    /// cypher gates its scans on [`base_visible`](Self::base_visible), impact
    /// redirects before it starts. A suppressed anchor would yield nothing
    /// here rather than a wrong answer, but it means "no edges", not "this
    /// node has no edges".
    #[inline]
    fn anchor(&self, idx: u32) -> Option<u32> {
        if idx < self.base_len() {
            return Some(idx);
        }
        self.view
            .and_then(|v| v.node(idx))
            .and_then(|n| n.replaced_base)
    }

    /// Outgoing edges of `idx`, base and overlay, already merged.
    #[inline]
    pub fn out_edges(&self, idx: u32) -> impl Iterator<Item = MergedEdge<'a>> + 'a {
        let graph = self.graph;
        let view = self.view;
        let base = self.anchor(idx).into_iter().flat_map(move |a| {
            let start = graph.out_offsets[a as usize].to_native() as usize;
            let end = graph.out_offsets[a as usize + 1].to_native() as usize;
            (start..end).filter_map(move |i| merge_base_edge(graph, view, i as u32))
        });
        let base_count = graph.edges.len() as u32;
        let overlay = view.into_iter().flat_map(move |v| {
            v.overlay_out(idx)
                .map(move |(i, e)| overlay_edge(base_count, i, e))
        });
        base.chain(overlay)
    }

    /// Incoming edges of `idx`, base and overlay, already merged.
    #[inline]
    pub fn in_edges(&self, idx: u32) -> impl Iterator<Item = MergedEdge<'a>> + 'a {
        let graph = self.graph;
        let view = self.view;
        let base = self.anchor(idx).into_iter().flat_map(move |a| {
            let start = graph.in_offsets[a as usize].to_native() as usize;
            let end = graph.in_offsets[a as usize + 1].to_native() as usize;
            (start..end)
                .filter_map(move |i| merge_base_edge(graph, view, graph.in_edge_idx[i].to_native()))
        });
        let base_count = graph.edges.len() as u32;
        let overlay = view.into_iter().flat_map(move |v| {
            v.overlay_in(idx)
                .map(move |(i, e)| overlay_edge(base_count, i, e))
        });
        base.chain(overlay)
    }

    /// Every edge in the graph, merged — for the "does any edge match" scans
    /// that have no endpoint to anchor on.
    #[inline]
    pub fn all_edges(&self) -> impl Iterator<Item = MergedEdge<'a>> + 'a {
        let graph = self.graph;
        let view = self.view;
        let base =
            (0..graph.edges.len() as u32).filter_map(move |i| merge_base_edge(graph, view, i));
        let base_count = graph.edges.len() as u32;
        let overlay = view.into_iter().flat_map(move |v| {
            v.edges()
                .iter()
                .enumerate()
                .map(move |(i, e)| overlay_edge(base_count, i as u32, e))
        });
        base.chain(overlay)
    }
}

#[inline]
fn overlay_edge(base_edge_count: u32, i: u32, e: &ViewEdge) -> MergedEdge<'_> {
    MergedEdge {
        idx: base_edge_count + i,
        source: e.source,
        target: e.target,
        origin: EdgeOrigin::Overlay(e),
    }
}

/// Apply the overlay to one base edge. `None` drops it.
///
/// The rule itself is [`OverlayView::masks_base_edge`]'s — see the
/// `mask ⊆ rebuild` section of [`crate::session::view`]. What is uniform here
/// is which index it is asked about: always the edge's SOURCE, in both
/// directions, because that is the file whose fresh parse can speak for it.
#[inline]
fn merge_base_edge<'a>(
    graph: &'a ArchivedZeroCopyGraph,
    view: Option<&'a OverlayView>,
    edge_idx: u32,
) -> Option<MergedEdge<'a>> {
    let edge = &graph.edges[edge_idx as usize];
    let source = edge.source.to_native();
    let target = edge.target.to_native();
    let Some(view) = view else {
        return Some(MergedEdge {
            idx: edge_idx,
            source,
            target,
            origin: EdgeOrigin::Base(edge),
        });
    };
    if view.masks_base_edge(source, RelType::from(&edge.rel_type)) {
        return None;
    }
    Some(MergedEdge {
        idx: edge_idx,
        source: view.redirect(source)?,
        target: view.redirect(target)?,
        origin: EdgeOrigin::Base(edge),
    })
}

impl<'a> MergedNode<'a> {
    pub fn kind(&self) -> NodeKind {
        match self {
            Self::Base(n) => NodeKind::from(&n.kind),
            Self::Virtual(n) => n.kind,
        }
    }

    pub fn uid(&self) -> u64 {
        match self {
            Self::Base(n) => n.uid.to_native(),
            Self::Virtual(n) => n.uid,
        }
    }

    pub fn name(&self, graph: &MergedGraph<'a>) -> &'a str {
        match self {
            Self::Base(n) => n.name.resolve(&graph.graph.string_pool),
            Self::Virtual(n) => n.name.as_str(),
        }
    }

    pub fn owner_class(&self, graph: &MergedGraph<'a>) -> Option<&'a str> {
        match self {
            Self::Base(n) => {
                let owner = n.owner_class.resolve(&graph.graph.string_pool);
                (!owner.is_empty()).then_some(owner)
            }
            Self::Virtual(n) => n.owner_class.as_deref(),
        }
    }

    pub fn file_path(&self, graph: &MergedGraph<'a>) -> Option<&'a str> {
        match self {
            Self::Base(n) => n.has_owning_file().then(|| {
                graph.graph.files[n.file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.graph.string_pool)
            }),
            Self::Virtual(n) => Some(&n.rel_path),
        }
    }

    pub fn start_line(&self) -> u32 {
        match self {
            Self::Base(n) => n.start_line(),
            Self::Virtual(n) => n.start_line,
        }
    }

    pub fn end_line(&self) -> u32 {
        match self {
            Self::Base(n) => n.end_line(),
            Self::Virtual(n) => n.end_line,
        }
    }

    /// Base index to consult for span-keyed side tables (function metas,
    /// content spans): the node itself when base, the uid-identical base twin
    /// when replaced, `None` for a brand-new symbol that has no metas yet.
    pub fn meta_idx(&self, idx: u32) -> Option<u32> {
        match self {
            Self::Base(_) => Some(idx),
            Self::Virtual(n) => n.replaced_base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ArchivedZeroCopyGraph;
    use crate::graph_fixture::GraphFixture;
    use crate::session::view::{OverlayFileInput, OverlaySymbol};
    use rkyv::rancor::Error as RkyvError;

    /// files: 0 = src/dirty.rs · 1 = src/clean.rs
    /// nodes: 0 keep_fn(dirty) · 1 gone_fn(dirty) · 2 clean_caller(clean)
    ///        3 target_fn(clean)
    /// edges: clean_caller→keep_fn, keep_fn→target_fn (Calls, stale),
    ///        keep_fn→target_fn (ReadsField), clean_caller→gone_fn
    fn base_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        let keep = fx.func("src/dirty.rs", "keep_fn");
        let gone = fx.func("src/dirty.rs", "gone_fn");
        let caller = fx.func("src/clean.rs", "clean_caller");
        let target = fx.func("src/clean.rs", "target_fn");
        fx.edge(caller, keep, RelType::Calls);
        fx.edge(keep, target, RelType::Calls);
        fx.edge(keep, target, RelType::ReadsField);
        fx.edge(caller, gone, RelType::Calls);
        fx.into_bytes()
    }

    /// The dirty file re-parsed: `keep_fn` survives and still calls
    /// `target_fn`; `gone_fn` is deleted.
    fn dirty_input() -> OverlayFileInput {
        OverlayFileInput {
            rel_path: "src/dirty.rs".to_string(),
            symbols: vec![OverlaySymbol {
                name: "keep_fn".to_string(),
                kind: NodeKind::Function,
                owner_class: None,
                start_line: 1,
                end_line: 2,
                calls: vec!["target_fn".to_string()],
            }],
            imports: vec![],
        }
    }

    fn with_view<F: FnOnce(MergedGraph<'_>, u32)>(f: F) {
        let bytes = base_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let keep_virt = view.base_len();
        f(MergedGraph::new(graph, Some(&view)), keep_virt);
    }

    #[test]
    fn without_a_view_edges_are_the_plain_csr() {
        let bytes = base_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let merged = MergedGraph::new(graph, None);

        let out: Vec<(u32, RelType)> = merged
            .out_edges(0)
            .map(|e| (e.target, e.rel_type()))
            .collect();
        assert_eq!(out, vec![(3, RelType::Calls), (3, RelType::ReadsField)]);
        let into: Vec<u32> = merged.in_edges(0).map(|e| e.source).collect();
        assert_eq!(into, vec![2]);
        assert_eq!(merged.all_edges().count(), graph.edges.len());
        assert_eq!(merged.node_count(), graph.nodes.len() as u32);
    }

    #[test]
    fn a_rebuilt_rel_is_masked_and_the_overlay_answers_instead() {
        with_view(|merged, keep_virt| {
            let out: Vec<(u32, RelType, bool)> = merged
                .out_edges(keep_virt)
                .map(|e| (e.target, e.rel_type(), e.is_overlay()))
                .collect();
            // The stale base Calls edge is gone; the overlay's own resolution
            // stands in for it. ReadsField has no fragment input, so its base
            // edge survives with the source redirected into merged space.
            assert!(
                out.contains(&(3, RelType::Calls, true)),
                "overlay Calls edge missing: {out:?}"
            );
            assert!(
                out.contains(&(3, RelType::ReadsField, false)),
                "non-rebuilt base rel must survive: {out:?}"
            );
            assert_eq!(
                out.iter()
                    .filter(|(_, r, ov)| *r == RelType::Calls && !ov)
                    .count(),
                0,
                "stale base Calls edge was not masked: {out:?}"
            );
        });
    }

    #[test]
    fn a_clean_caller_reaches_the_virtual_node_not_its_stale_twin() {
        with_view(|merged, keep_virt| {
            let into: Vec<(u32, u32)> = merged
                .in_edges(keep_virt)
                .map(|e| (e.source, e.target))
                .collect();
            assert_eq!(into, vec![(2, keep_virt)]);
        });
    }

    #[test]
    fn edges_touching_a_deleted_symbol_are_dropped() {
        with_view(|merged, keep_virt| {
            // gone_fn is base index 1, suppressed by the fresh parse.
            assert_eq!(merged.in_edges(1).count(), 0);
            assert!(!merged.base_visible(1));
            // and it is unreachable from the clean caller's out-edges too.
            let out: Vec<u32> = merged.out_edges(2).map(|e| e.target).collect();
            assert_eq!(out, vec![keep_virt]);
        });
    }

    #[test]
    fn all_edges_sees_both_sides_of_the_merge() {
        with_view(|merged, keep_virt| {
            let edges: Vec<(u32, u32, RelType, bool)> = merged
                .all_edges()
                .map(|e| (e.source, e.target, e.rel_type(), e.is_overlay()))
                .collect();
            assert!(edges.contains(&(2, keep_virt, RelType::Calls, false)));
            assert!(edges.contains(&(keep_virt, 3, RelType::ReadsField, false)));
            assert!(edges.contains(&(keep_virt, 3, RelType::Calls, true)));
            // the deleted symbol's edge and the masked stale Calls edge are
            // both absent.
            assert_eq!(edges.len(), 3, "unexpected merged edge set: {edges:?}");
        });
    }

    #[test]
    fn a_node_reads_the_same_either_side_of_the_merge() {
        with_view(|merged, keep_virt| {
            let base = merged.node(2).expect("clean node");
            assert_eq!(base.name(&merged), "clean_caller");
            assert_eq!(base.file_path(&merged), Some("src/clean.rs"));
            assert_eq!(base.meta_idx(2), Some(2));

            let virt = merged.node(keep_virt).expect("virtual node");
            assert_eq!(virt.name(&merged), "keep_fn");
            assert_eq!(virt.kind(), NodeKind::Function);
            assert_eq!(virt.file_path(&merged), Some("src/dirty.rs"));
            // side tables are span-keyed on the base twin, not the virtual index
            assert_eq!(virt.meta_idx(keep_virt), Some(0));
        });
    }

    #[test]
    fn a_merged_edge_index_still_addresses_its_edge() {
        with_view(|merged, keep_virt| {
            for edge in merged.out_edges(keep_virt) {
                match merged.overlay_edge(edge.idx) {
                    Some(view_edge) => {
                        assert!(edge.is_overlay());
                        assert_eq!(view_edge.target, edge.target);
                        assert_eq!(view_edge.rel_type, edge.rel_type());
                    }
                    None => {
                        assert!(!edge.is_overlay());
                        assert!((edge.idx as usize) < merged.edges.len());
                    }
                }
            }
        });
    }

    #[test]
    fn an_overlay_edge_reports_its_own_reason() {
        let bytes = base_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let merged = MergedGraph::new(graph, Some(&view));
        let keep_virt = view.base_len();

        for edge in merged.out_edges(keep_virt) {
            let expected = if edge.is_overlay() {
                OVERLAY_EDGE_REASON
            } else {
                "ast-call"
            };
            assert_eq!(edge.reason(graph), expected);
        }
    }
}
