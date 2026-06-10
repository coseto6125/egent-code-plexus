//! Query-time L1 overlay merge view (FU-2026-06-10-398a846bca42 root cure).
//!
//! `OverlayView` makes uncommitted-edit symbols AND edges visible to graph
//! traversals (impact, cypher) without rebuilding L2 and without loading
//! O(graph) data. The base `ArchivedZeroCopyGraph` stays an immutable mmap;
//! the view is a small in-memory delta strictly O(dirty files):
//!
//! - **virtual nodes** — every symbol in a dirty file, addressed by virtual
//!   index `base_len + i` so they compose with the base graph's dense `u32`
//!   node-index space that edges and traversals already use.
//! - **replaced** — a dirty-file symbol whose uid (kind + path + owner +
//!   name) matches a base node: the base node's identity survives the edit,
//!   so traversal redirects base index → virtual index (spans/lines come
//!   from the on-disk version; base IN-edges from clean files stay valid).
//! - **suppressed** — base nodes of dirty files NOT re-emitted by the fresh
//!   parse: deleted or renamed. Traversal must neither expand into nor
//!   report them — this is what kills phantom callers after a rename.
//! - **overlay edges** — `Calls` edges re-resolved from each dirty symbol's
//!   callee names, mirroring index-time Pass-2 tier semantics (same-file →
//!   import-scoped → unique-global with `AmbiguousGlobal` suppression)
//!   against the archived `name_index` (O(log N) per lookup, no allocation
//!   proportional to the graph).
//!
//! ## The masking invariant
//!
//! A base edge whose **source** lies in a dirty file is masked — that file
//! was re-parsed and its outgoing truth is the overlay adjacency. A base
//! edge whose **target** lies in a dirty file is redirected (replaced) or
//! dropped (suppressed). Clean↔clean edges are untouched. Consumers apply
//! this via [`OverlayView::base_source_masked`] + [`OverlayView::redirect`].
//!
//! ## What stays out of scope (documented fidelity gaps)
//!
//! - Heuristic `References` edges from `fanout_refs` are not re-resolved
//!   (impact filters heuristic edges by default; the loss is bounded).
//! - Containment / file-level edges for virtual nodes are not synthesized.
//! - Clean files calling a name that only NOW resolves (new symbol breaks a
//!   previous ambiguity) keep their index-time resolution — clean files are
//!   never re-resolved at query time.

use crate::analyzer::types::RawImport;
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// One symbol from a freshly parsed dirty file.
#[derive(Debug, Clone)]
pub struct OverlaySymbol {
    pub name: String,
    pub kind: NodeKind,
    pub owner_class: Option<String>,
    /// 1-based, matching `Node::start_line` conventions.
    pub start_line: u32,
    pub end_line: u32,
    /// Callee short names invoked inside this symbol's body (RawNode.calls).
    pub calls: Vec<String>,
}

/// One dirty file's fresh parse, as loaded from a v2 fragment bin.
#[derive(Debug, Clone)]
pub struct OverlayFileInput {
    /// Repo-relative path, same normalization as `File.path` in the graph.
    pub rel_path: String,
    pub symbols: Vec<OverlaySymbol>,
    pub imports: Vec<RawImport>,
}

/// A materialized overlay node. `rel_path` is shared per file via `Arc`.
#[derive(Debug, Clone)]
pub struct ViewNode {
    pub uid: u64,
    pub name: String,
    pub kind: NodeKind,
    pub owner_class: Option<String>,
    pub rel_path: Arc<str>,
    pub start_line: u32,
    pub end_line: u32,
    /// `Some(base_idx)` when this node replaces a surviving base node —
    /// traversal reads the base CSR IN-edges of `base_idx` (clean callers)
    /// in addition to [`OverlayView::overlay_in`].
    pub replaced_base: Option<u32>,
}

/// An overlay-resolved edge. Indices are merged-space: `< base_len` = base
/// node, `>= base_len` = virtual node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewEdge {
    pub source: u32,
    pub target: u32,
    pub rel_type: RelType,
    pub confidence: f32,
}

/// Tier confidences mirroring index-time Pass-2 resolution.
const CONF_SAME_FILE: f32 = 1.0;
const CONF_IMPORT_SCOPED: f32 = 0.95;
const CONF_GLOBAL_UNIQUE: f32 = 0.7;

#[derive(Debug, Default)]
pub struct OverlayView {
    base_len: u32,
    nodes: Vec<ViewNode>,
    /// base idx → virtual idx for surviving (uid-matched) symbols.
    replaced: FxHashMap<u32, u32>,
    /// base idxs of dirty-file symbols NOT re-emitted (deleted/renamed).
    suppressed: FxHashSet<u32>,
    /// every base idx living in a dirty file (replaced ∪ suppressed):
    /// base edges sourced here are masked in favour of overlay adjacency.
    dirty_base: FxHashSet<u32>,
    out_adj: FxHashMap<u32, Vec<ViewEdge>>,
    in_adj: FxHashMap<u32, Vec<ViewEdge>>,
}

impl OverlayView {
    /// Build the view. Returns `None` when `files` is empty — the clean-tree
    /// path must stay literally view-free so traversals take their original
    /// branch. Cost: one pass over `graph.nodes` to find dirty-file base
    /// nodes (no per-node allocation for clean files), then O(dirty symbols
    /// × callees × log N) name-index lookups.
    pub fn build(graph: &ArchivedZeroCopyGraph, files: &[OverlayFileInput]) -> Option<Self> {
        if files.is_empty() {
            return None;
        }
        let base_len = graph.nodes.len() as u32;

        // ── dirty file_idx set ────────────────────────────────────────────
        let rel_paths: FxHashSet<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        let dirty_file_idx: FxHashSet<u32> = graph
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| rel_paths.contains(f.path.resolve(&graph.string_pool)))
            .map(|(i, _)| i as u32)
            .collect();

        // ── base nodes living in dirty files ──────────────────────────────
        // Single O(N) scan, collecting only matches. uid → base idx lets the
        // virtual-node pass detect survivors exactly (uid embeds kind, path,
        // owner_class and name — Fragment v2 carries owner_class so methods
        // match too, see overlay_writer).
        let mut dirty_base_by_uid: FxHashMap<u64, u32> = FxHashMap::default();
        let mut dirty_base: FxHashSet<u32> = FxHashSet::default();
        if !dirty_file_idx.is_empty() {
            for (i, node) in graph.nodes.iter().enumerate() {
                if dirty_file_idx.contains(&node.file_idx.to_native()) {
                    dirty_base_by_uid.insert(node.uid.to_native(), i as u32);
                    dirty_base.insert(i as u32);
                }
            }
        }

        // ── virtual nodes + replaced mapping ──────────────────────────────
        let mut nodes: Vec<ViewNode> = Vec::new();
        let mut replaced: FxHashMap<u32, u32> = FxHashMap::default();

        for file in files {
            let rel: Arc<str> = Arc::from(file.rel_path.as_str());
            for sym in &file.symbols {
                let uid = crate::uid::compute(
                    sym.kind,
                    &file.rel_path,
                    sym.owner_class.as_deref(),
                    &sym.name,
                );
                let virt = base_len + nodes.len() as u32;
                if let Some(&base_idx) = dirty_base_by_uid.get(&uid) {
                    replaced.insert(base_idx, virt);
                }
                nodes.push(ViewNode {
                    uid,
                    name: sym.name.clone(),
                    kind: sym.kind,
                    owner_class: sym.owner_class.clone(),
                    rel_path: rel.clone(),
                    start_line: sym.start_line,
                    end_line: sym.end_line,
                    replaced_base: dirty_base_by_uid.get(&uid).copied(),
                });
            }
        }
        let suppressed: FxHashSet<u32> = dirty_base
            .iter()
            .copied()
            .filter(|idx| !replaced.contains_key(idx))
            .collect();

        // ── overlay Calls edges ───────────────────────────────────────────
        // Inner scope: the name maps borrow `nodes`' strings and must drop
        // before `nodes` moves into Self.
        let mut out_adj: FxHashMap<u32, Vec<ViewEdge>> = FxHashMap::default();
        let mut in_adj: FxHashMap<u32, Vec<ViewEdge>> = FxHashMap::default();
        {
            // (file ordinal, name) → virtual idxs of callable symbols: Tier 1.
            let mut same_file_callables: FxHashMap<(usize, &str), Vec<u32>> = FxHashMap::default();
            // name → virtual idxs of callable symbols anywhere in the
            // overlay: participates in Tier-3 global-uniqueness counting.
            let mut overlay_callables: FxHashMap<&str, Vec<u32>> = FxHashMap::default();

            let mut virt_off = 0usize;
            for (file_ord, file) in files.iter().enumerate() {
                for _ in &file.symbols {
                    let node = &nodes[virt_off];
                    if node.kind.is_callable() {
                        let virt = base_len + virt_off as u32;
                        same_file_callables
                            .entry((file_ord, node.name.as_str()))
                            .or_default()
                            .push(virt);
                        overlay_callables
                            .entry(node.name.as_str())
                            .or_default()
                            .push(virt);
                    }
                    virt_off += 1;
                }
            }

            let mut push_edge = |edge: ViewEdge| {
                out_adj.entry(edge.source).or_default().push(edge);
                in_adj.entry(edge.target).or_default().push(edge);
            };

            let mut virt_cursor = base_len;
            for (file_ord, file) in files.iter().enumerate() {
                for sym in &file.symbols {
                    let source = virt_cursor;
                    virt_cursor += 1;
                    for callee in &sym.calls {
                        if let Some((target, confidence)) = resolve_callee(
                            graph,
                            callee,
                            file_ord,
                            file,
                            &same_file_callables,
                            &overlay_callables,
                            &replaced,
                            &dirty_base,
                        ) {
                            push_edge(ViewEdge {
                                source,
                                target,
                                rel_type: RelType::Calls,
                                confidence,
                            });
                        }
                    }
                }
            }
        }

        Some(Self {
            base_len,
            nodes,
            replaced,
            suppressed,
            dirty_base,
            out_adj,
            in_adj,
        })
    }

    pub fn base_len(&self) -> u32 {
        self.base_len
    }

    /// All virtual nodes; index `i` here is merged index `base_len + i`.
    pub fn virtual_nodes(&self) -> &[ViewNode] {
        &self.nodes
    }

    /// Virtual-index accessor. `None` for base indices.
    pub fn node(&self, idx: u32) -> Option<&ViewNode> {
        idx.checked_sub(self.base_len)
            .and_then(|i| self.nodes.get(i as usize))
    }

    /// Where traversal should actually go when it lands on `idx`:
    /// suppressed base → `None` (node no longer exists on disk), replaced
    /// base → its virtual successor, anything else → itself.
    pub fn redirect(&self, idx: u32) -> Option<u32> {
        if self.suppressed.contains(&idx) {
            return None;
        }
        Some(self.replaced.get(&idx).copied().unwrap_or(idx))
    }

    /// True when base edges SOURCED at `base_idx` must be ignored — the
    /// owning file was re-parsed and `overlay_out` carries its truth.
    pub fn base_source_masked(&self, base_idx: u32) -> bool {
        self.dirty_base.contains(&base_idx)
    }

    /// Overlay-resolved outgoing edges of `idx` (virtual sources only —
    /// clean base nodes gain no new outgoing edges).
    pub fn overlay_out(&self, idx: u32) -> &[ViewEdge] {
        self.out_adj.get(&idx).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Overlay-resolved incoming edges of `idx` (current callers living in
    /// dirty files). For a replaced virtual node, base CSR IN-edges of its
    /// `replaced_base` are ALSO valid — consumers read both.
    pub fn overlay_in(&self, idx: u32) -> &[ViewEdge] {
        self.in_adj.get(&idx).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Mirror of index-time Pass-2 `Calls` resolution, narrowed to the inputs
/// available at query time. Returns the merged-space target index.
///
/// Tier 1 — same file: the dirty file was FULLY re-parsed, so its own
/// callable set is authoritative. Unique match → confidence 1.0.
/// Tier 2 — import-scoped: callee name appears as an import's name/alias;
/// candidates narrowed to files matching the import source's last path
/// segment. Unique → 0.95.
/// Tier 3 — global unique: all clean-base callables (via the archived
/// `name_index`) plus all overlay callables. ≥2 candidates → suppressed,
/// matching `DecisionTier::AmbiguousGlobal` (an invented edge is worse than
/// a missing one). Unique → 0.7.
#[allow(clippy::too_many_arguments)]
fn resolve_callee(
    graph: &ArchivedZeroCopyGraph,
    callee: &str,
    file_ord: usize,
    file: &OverlayFileInput,
    same_file_callables: &FxHashMap<(usize, &str), Vec<u32>>,
    overlay_callables: &FxHashMap<&str, Vec<u32>>,
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
) -> Option<(u32, f32)> {
    // Tier 1: same-file.
    if let Some(virts) = same_file_callables.get(&(file_ord, callee)) {
        if virts.len() == 1 {
            return Some((virts[0], CONF_SAME_FILE));
        }
        // Ambiguous within one file (overloads): suppress, like index time.
        return None;
    }

    // Clean-base candidates via the name index. Dirty-file base nodes are
    // excluded here: replaced ones already participate as overlay callables
    // (same name), suppressed ones no longer exist.
    let base_candidates: Vec<u32> = graph
        .nodes_by_name(callee)
        .filter(|&idx| {
            NodeKind::from(&graph.nodes[idx as usize].kind).is_callable()
                && !dirty_base.contains(&idx)
        })
        .collect();
    let overlay_candidates: &[u32] = overlay_callables
        .get(callee)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    // Tier 2: import-scoped.
    if let Some(import) = file
        .imports
        .iter()
        .find(|i| i.imported_name == callee || i.alias.as_deref() == Some(callee))
    {
        let segment = import_last_segment(&import.source);
        if !segment.is_empty() {
            let scoped: Vec<u32> = base_candidates
                .iter()
                .copied()
                .filter(|&idx| {
                    let file_idx = graph.nodes[idx as usize].file_idx.to_native() as usize;
                    graph
                        .files
                        .get(file_idx)
                        .map(|f| path_matches_segment(f.path.resolve(&graph.string_pool), segment))
                        .unwrap_or(false)
                })
                .collect();
            if scoped.len() == 1 {
                return Some((redirect_target(scoped[0], replaced), CONF_IMPORT_SCOPED));
            }
        }
    }

    // Tier 3: global uniqueness over clean base + overlay.
    match (base_candidates.len(), overlay_candidates.len()) {
        (1, 0) => Some((
            redirect_target(base_candidates[0], replaced),
            CONF_GLOBAL_UNIQUE,
        )),
        (0, 1) => Some((overlay_candidates[0], CONF_GLOBAL_UNIQUE)),
        _ => None,
    }
}

/// Replaced base targets redirect to their virtual successor so edge
/// endpoints always reference the live version of a symbol.
fn redirect_target(base_idx: u32, replaced: &FxHashMap<u32, u32>) -> u32 {
    replaced.get(&base_idx).copied().unwrap_or(base_idx)
}

/// Last path-ish segment of an import source across language conventions:
/// `./utils/helpers` → `helpers`, `crate::foo::bar` → `bar`, `pkg.mod` →
/// `mod`. Empty when the source is all separators.
fn import_last_segment(source: &str) -> &str {
    source
        .rsplit(['/', '.', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

/// Does `rel_path` plausibly serve `segment` as a module? Matches the file
/// stem or any directory component — a deliberate approximation of import
/// resolution that errs toward "no unique match" (Tier 3 then decides).
fn path_matches_segment(rel_path: &str, segment: &str) -> bool {
    std::path::Path::new(rel_path)
        .components()
        .any(|c| match c.as_os_str().to_str() {
            Some(s) => {
                s == segment
                    || s.strip_suffix(".rs")
                        .map(|stem| stem == segment)
                        .unwrap_or(false)
                    || {
                        std::path::Path::new(s)
                            .file_stem()
                            .and_then(|st| st.to_str())
                            .map(|st| st == segment)
                            .unwrap_or(false)
                    }
            }
            None => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::types::RawImport;
    use crate::graph::{
        Edge, File, FileCategory, NameIndexEntry, Node, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
        GRAPH_MAGIC,
    };
    use crate::pool::StringPool;
    use rkyv::rancor::Error as RkyvError;

    /// Fixture: a base graph with a real `name_index` and uid-correct nodes.
    ///
    /// files:  0 = src/clean.rs · 1 = src/dirty.rs · 2 = src/other.rs
    /// nodes:  0 keep_fn(dirty) · 1 gone_fn(dirty) · 2 callee_unique(clean)
    ///         3 dup_fn(clean) · 4 dup_fn(other) · 5 imp_fn(other)
    ///         6 imp_fn(clean)
    fn base_graph_bytes() -> Vec<u8> {
        let mut pool = StringPool::new();
        let paths = ["src/clean.rs", "src/dirty.rs", "src/other.rs"];
        let files: Vec<File> = paths
            .iter()
            .map(|p| File {
                path: pool.add(p),
                mtime: 0,
                content_hash: [0; 8],
                category: FileCategory::Source,
            })
            .collect();

        let specs: &[(&str, usize)] = &[
            ("keep_fn", 1),
            ("gone_fn", 1),
            ("callee_unique", 0),
            ("dup_fn", 0),
            ("dup_fn", 2),
            ("imp_fn", 2),
            ("imp_fn", 0),
        ];
        let nodes: Vec<Node> = specs
            .iter()
            .map(|(name, file_idx)| Node {
                uid: crate::uid::compute(NodeKind::Function, paths[*file_idx], None, name),
                name: pool.add(name),
                file_idx: *file_idx as u32,
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                community_id: 0,
                owner_class: crate::pool::StrRef::default(),
                content_hash: 0,
            })
            .collect();

        let mut name_index: Vec<NameIndexEntry> = specs
            .iter()
            .enumerate()
            .map(|(i, (name, _))| NameIndexEntry {
                name_hash: crate::uid::xxh3_64_bytes(name.as_bytes()),
                node_idx: i as u32,
            })
            .collect();
        name_index.sort_unstable_by_key(|e| e.name_hash);

        let n = nodes.len();
        let g = ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes.clone(),
            files,
            nodes,
            edges: Vec::<Edge>::new(),
            out_offsets: vec![0u32; n + 1],
            in_offsets: vec![0u32; n + 1],
            in_edge_idx: vec![],
            name_index,
            process_start: 0,
            traces_offsets: vec![0],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
            node_flags: vec![],
        };
        rkyv::to_bytes::<RkyvError>(&g)
            .expect("serialize")
            .into_vec()
    }

    fn sym(name: &str, calls: &[&str]) -> OverlaySymbol {
        OverlaySymbol {
            name: name.to_string(),
            kind: NodeKind::Function,
            owner_class: None,
            start_line: 1,
            end_line: 2,
            calls: calls.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn dirty_input() -> OverlayFileInput {
        OverlayFileInput {
            rel_path: "src/dirty.rs".to_string(),
            symbols: vec![
                sym(
                    "keep_fn",
                    &["callee_unique", "dup_fn", "imp_fn", "new_helper"],
                ),
                sym("new_helper", &["keep_fn"]),
            ],
            imports: vec![RawImport {
                source: "crate::other".to_string(),
                imported_name: "imp_fn".to_string(),
                alias: None,
                binding_kind: None,
            }],
        }
    }

    fn edge_to(view: &OverlayView, source: u32, target: u32) -> Option<ViewEdge> {
        view.overlay_out(source)
            .iter()
            .find(|e| e.target == target)
            .copied()
    }

    #[test]
    fn empty_input_builds_no_view() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        assert!(OverlayView::build(graph, &[]).is_none());
    }

    #[test]
    fn replaced_suppressed_and_masking() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let base_len = view.base_len();

        // keep_fn survives the edit: base 0 redirects to its virtual twin.
        assert_eq!(view.redirect(0), Some(base_len));
        assert_eq!(view.node(base_len).unwrap().replaced_base, Some(0));
        // gone_fn was deleted: traversal must drop it entirely.
        assert_eq!(view.redirect(1), None);
        // both dirty-file base nodes mask their stale outgoing base edges.
        assert!(view.base_source_masked(0));
        assert!(view.base_source_masked(1));
        // clean nodes keep their base edges.
        assert!(!view.base_source_masked(2));
        assert_eq!(view.redirect(2), Some(2));
    }

    #[test]
    fn tier1_same_file_edge() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let keep_virt = view.base_len();
        let helper_virt = view.base_len() + 1;

        // keep_fn → new_helper and new_helper → keep_fn, both same-file.
        let e1 = edge_to(&view, keep_virt, helper_virt).expect("keep_fn → new_helper");
        assert_eq!(e1.confidence, 1.0);
        let e2 = edge_to(&view, helper_virt, keep_virt).expect("new_helper → keep_fn");
        assert_eq!(e2.confidence, 1.0);
        assert_eq!(e2.rel_type, RelType::Calls);
    }

    #[test]
    fn tier3_unique_global_resolves_ambiguous_suppresses() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let keep_virt = view.base_len();

        // callee_unique: one global candidate → edge with Tier-3 confidence.
        let e = edge_to(&view, keep_virt, 2).expect("keep_fn → callee_unique");
        assert_eq!(e.confidence, 0.7);
        // dup_fn: two clean-base candidates → AmbiguousGlobal-style suppression.
        assert!(edge_to(&view, keep_virt, 3).is_none());
        assert!(edge_to(&view, keep_virt, 4).is_none());
        // the reverse index sees the resolved caller.
        assert!(view.overlay_in(2).iter().any(|e| e.source == keep_virt));
    }

    #[test]
    fn tier2_import_scoped_beats_global_ambiguity() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let keep_virt = view.base_len();

        // imp_fn exists in other.rs AND clean.rs (globally ambiguous), but
        // the import source `crate::other` scopes it to other.rs (node 5).
        let e = edge_to(&view, keep_virt, 5).expect("keep_fn → imp_fn via import");
        assert_eq!(e.confidence, 0.95);
        assert!(edge_to(&view, keep_virt, 6).is_none());
    }

    #[test]
    fn edge_into_replaced_base_redirects_to_virtual() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        // Second dirty file calls keep_fn — globally unique among callables
        // once dirty-base copies are excluded; target must be keep_fn's
        // VIRTUAL index, never the stale base node.
        let other_dirty = OverlayFileInput {
            rel_path: "src/clean.rs".to_string(),
            symbols: vec![sym("caller_two", &["keep_fn"])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[dirty_input(), other_dirty]).unwrap();
        let keep_virt = view.base_len();
        let caller_two_virt = view.base_len() + 2;

        let e = edge_to(&view, caller_two_virt, keep_virt)
            .expect("caller_two → keep_fn must redirect to the virtual node");
        assert_eq!(e.target, keep_virt);
        assert!(view
            .overlay_in(keep_virt)
            .iter()
            .any(|e| e.source == caller_two_virt));
    }
}
