use crate::graph::{ArchivedNode, ArchivedZeroCopyGraph, Node};
use crate::registry::io::atomic_write_json;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyFiles {
    pub version: u32,
    #[serde(default)]
    pub entries: BTreeMap<String, DirtyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyEntry {
    pub mtime_ns: u64,
    pub content_hash: String,
    pub fragment_id: String,
    pub tantivy_delta_segment: Option<String>,
    #[serde(default)]
    pub parse_failed: bool,
    #[serde(default)]
    pub dirty_symbols: Vec<SymbolRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRef {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Const,
    Type,
    Module,
    /// Fallback when the analyzer returns a kind string we don't map (cross-language tail).
    Unknown,
}

impl DirtyFiles {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
    pub fn empty() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
        }
    }
}

/// In-memory overlay container: nodes that override or extend the base graph.
///
/// T7-5: `merge_archived` uses `ArchivedOverlay.nodes` as the overlay set.
/// No tombstone support in v1 — deletions are out of scope until T7-6.
/// See `merge_archived` doc-comment for the exact merge semantics.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, Default)]
#[rkyv(derive(Debug))]
pub struct Overlay {
    pub nodes: Vec<Node>,
}

impl Overlay {
    pub fn new(nodes: Vec<Node>) -> Self {
        Self { nodes }
    }
}

/// Merge `base` and `overlay` into a single node iterator.
///
/// Semantics:
/// - Every overlay node is yielded first (in overlay order).
/// - Every base node whose `uid` does not appear in the overlay is yielded
///   afterward.
/// - Each uid appears **exactly once** in the output.
/// - Overlay wins on uid collision: the overlay node replaces the base node.
///
/// Zero allocs per iteration: `FxHashSet<u64>` is built once at
/// call time from the overlay uids, then only `contains` is called per
/// base node.
///
/// **Tombstone / deletion**: not supported in v1. The overlay has no
/// tombstone mechanism; base nodes can only be overridden (uid collision),
/// not deleted. T7-6 may add a `deleted_uids: Vec<u64>` field to `Overlay`
/// and extend this function accordingly.
pub fn merge_archived<'a>(
    base: &'a ArchivedZeroCopyGraph,
    overlay: &'a ArchivedOverlay,
) -> MergeIter<'a> {
    // One allocation: build the uid set from overlay nodes.
    let overlay_uids: FxHashSet<u64> = overlay.nodes.iter().map(|n| n.uid.to_native()).collect();

    MergeIter {
        overlay_nodes: overlay.nodes.as_slice(),
        base_nodes: base.nodes.as_slice(),
        overlay_uids,
        phase: 0,
        idx: 0,
    }
}

/// Merge iterator state.
///
/// Constructed once per `merge_archived` call. The `overlay_uids`
/// `FxHashSet<u64>` is built at construction time — **one allocation** — and
/// is then read-only during iteration. All subsequent per-item work is a
/// single `FxHashSet::contains` call: O(1) with zero allocations.
pub struct MergeIter<'a> {
    // Overlay nodes first, then filtered base nodes.
    overlay_nodes: &'a [ArchivedNode],
    base_nodes: &'a [ArchivedNode],
    overlay_uids: FxHashSet<u64>,
    /// Current position: phase 0 = overlay_nodes, phase 1 = base_nodes.
    phase: u8,
    idx: usize,
}

impl<'a> Iterator for MergeIter<'a> {
    type Item = &'a ArchivedNode;

    fn next(&mut self) -> Option<Self::Item> {
        if self.phase == 0 {
            // Yield every overlay node.
            if self.idx < self.overlay_nodes.len() {
                let node = &self.overlay_nodes[self.idx];
                self.idx += 1;
                return Some(node);
            }
            self.phase = 1;
            self.idx = 0;
        }
        // phase == 1: yield base nodes whose uid is not in the overlay set.
        loop {
            if self.idx >= self.base_nodes.len() {
                return None;
            }
            let node = &self.base_nodes[self.idx];
            self.idx += 1;
            if !self.overlay_uids.contains(&node.uid.to_native()) {
                return Some(node);
            }
        }
    }
}
