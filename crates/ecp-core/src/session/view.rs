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
//! ## The masking invariant: mask ⊆ rebuild
//!
//! A base edge whose **source** lies in a dirty file is masked ONLY when the
//! overlay re-resolves that rel type from fragment data ([`REBUILT_RELS`],
//! today `Calls`) — for those rels the overlay adjacency is the file's
//! truth. Rels the fragment carries no inputs for (ReadsField, Extends, …)
//! keep their base edges: slightly stale beats silently absent for a
//! deterministic edge. A base edge whose **target** lies in a dirty file is
//! redirected (replaced) or dropped (suppressed) regardless of rel.
//! Clean↔clean edges are untouched. Consumers apply this via
//! [`OverlayView::masks_base_edge`] + [`OverlayView::redirect`].
//!
//! ## What stays out of scope (documented fidelity gaps)
//!
//! - Heuristic `References` edges from `fanout_refs` are not re-resolved
//!   (impact filters heuristic edges by default; the loss is bounded).
//! - Containment / file-level edges for virtual nodes are not synthesized.
//! - Clean files calling a name that only NOW resolves (new symbol breaks a
//!   previous ambiguity) keep their index-time resolution — clean files are
//!   never re-resolved at query time.

use crate::analyzer::types::{CallSite, RawImport};
use crate::file_category::{pick_global, FileMeta, GlobalPick};
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
    /// Flat overlay-edge store; adjacency maps hold indices into it so
    /// consumers (cypher edge vars) can address an overlay edge by a stable
    /// virtual edge index (`graph.edges.len() + i`).
    edges: Vec<ViewEdge>,
    out_adj: FxHashMap<u32, Vec<u32>>,
    in_adj: FxHashMap<u32, Vec<u32>>,
}

impl OverlayView {
    /// Build the view. Returns `None` when `files` is empty — the clean-tree
    /// path must stay literally view-free so traversals take their original
    /// branch. Cost when dirty: one O(files) pass over `graph.files` plus one
    /// O(nodes) pass over `graph.nodes` (pure scans; allocation stays O(dirty
    /// symbols)), then O(dirty symbols × callees × log N) name-index lookups.
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
                let replaced_base = dirty_base_by_uid.get(&uid).copied();
                if let Some(base_idx) = replaced_base {
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
                    replaced_base,
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
        let mut edges: Vec<ViewEdge> = Vec::new();
        let mut out_adj: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        let mut in_adj: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        {
            let mut callables = OverlayNames::new(NodeKind::is_callable);
            let mut types = OverlayNames::new(NodeKind::is_type);
            // Base-file meta, filled on first sight: a common callee name has
            // hundreds of candidates spread over far fewer files.
            let mut base_metas: FxHashMap<usize, FileMeta> = FxHashMap::default();
            let file_metas: Vec<FileMeta> = files
                .iter()
                .map(|f| FileMeta::from_path(&f.rel_path))
                .collect();

            let mut virt_off = 0usize;
            for (file_ord, file) in files.iter().enumerate() {
                for _ in &file.symbols {
                    let node = &nodes[virt_off];
                    let virt = base_len + virt_off as u32;
                    callables.add(file_ord, node, virt, file_metas[file_ord]);
                    types.add(file_ord, node, virt, file_metas[file_ord]);
                    virt_off += 1;
                }
            }

            let mut push_edge = |edge: ViewEdge| {
                // Traversal consumers (impact's run_bfs) rely on overlay
                // sources being virtual: a base-space source would bypass
                // the masking invariant and resurrect stale base adjacency.
                debug_assert!(
                    edge.source >= base_len,
                    "overlay edge sources must be virtual indices"
                );
                let ei = edges.len() as u32;
                edges.push(edge);
                out_adj.entry(edge.source).or_default().push(ei);
                in_adj.entry(edge.target).or_default().push(ei);
            };

            let mut virt_cursor = base_len;
            for (file_ord, file) in files.iter().enumerate() {
                for sym in &file.symbols {
                    let source = virt_cursor;
                    virt_cursor += 1;
                    for raw_callee in &sym.calls {
                        let site = CallSite::parse(raw_callee);
                        let caller = file_metas[file_ord];
                        let hit = resolve_callee(
                            graph,
                            site.name(),
                            file_ord,
                            file,
                            caller,
                            &mut base_metas,
                            &callables,
                            &replaced,
                            &dirty_base,
                        )
                        .or_else(|| {
                            resolve_instantiation(
                                graph,
                                site,
                                file_ord,
                                file,
                                caller,
                                &mut base_metas,
                                &types,
                                &nodes,
                                &replaced,
                                &dirty_base,
                            )
                        });
                        if let Some((target, confidence)) = hit {
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
            edges,
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

    /// True when a base edge of type `rel` SOURCED at `base_idx` must be
    /// ignored: the owning file was re-parsed AND the overlay rebuilds this
    /// rel (`overlay_out` carries its truth). Masking a rel the overlay
    /// can't rebuild would silently drop deterministic edges — keep
    /// mask ⊆ rebuild as fragment capabilities grow.
    pub fn masks_base_edge(&self, base_idx: u32, rel: RelType) -> bool {
        // REBUILT_RELS: extend alongside resolve_callee when fragments gain
        // inputs for more rel types (e.g. field_reads → ReadsField).
        matches!(rel, RelType::Calls) && self.dirty_base.contains(&base_idx)
    }

    /// All overlay edges; index `i` here is overlay edge index `i`, addressed
    /// by consumers as merged edge index `graph.edges.len() + i`.
    pub fn edges(&self) -> &[ViewEdge] {
        &self.edges
    }

    /// Overlay-edge accessor by overlay edge index (NOT merged index —
    /// callers subtract `graph.edges.len()` first).
    pub fn edge(&self, overlay_edge_idx: u32) -> Option<&ViewEdge> {
        self.edges.get(overlay_edge_idx as usize)
    }

    /// Overlay-resolved outgoing edges of `idx` (virtual sources only —
    /// clean base nodes gain no new outgoing edges). Yields
    /// `(overlay_edge_idx, edge)`.
    pub fn overlay_out(&self, idx: u32) -> impl Iterator<Item = (u32, &ViewEdge)> + '_ {
        self.out_adj
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|&ei| (ei, &self.edges[ei as usize]))
    }

    /// Overlay-resolved incoming edges of `idx` (current callers living in
    /// dirty files). For a replaced virtual node, base CSR IN-edges of its
    /// `replaced_base` are ALSO valid — consumers read both. Yields
    /// `(overlay_edge_idx, edge)`.
    pub fn overlay_in(&self, idx: u32) -> impl Iterator<Item = (u32, &ViewEdge)> + '_ {
        self.in_adj
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|&ei| (ei, &self.edges[ei as usize]))
    }
}

/// Overlay symbols of one kind family (`kind`), by name: the Tier-1
/// same-file and Tier-3 overlay-wide candidates of [`resolve_callee`].
struct OverlayNames<'a> {
    kind: fn(NodeKind) -> bool,
    /// (file ordinal, name) → virtual idxs.
    same_file: FxHashMap<(usize, &'a str), Vec<u32>>,
    /// name → (virtual idx, file meta), anywhere in the overlay.
    anywhere: FxHashMap<&'a str, Vec<(u32, FileMeta)>>,
}

impl<'a> OverlayNames<'a> {
    fn new(kind: fn(NodeKind) -> bool) -> Self {
        Self {
            kind,
            same_file: FxHashMap::default(),
            anywhere: FxHashMap::default(),
        }
    }

    fn add(&mut self, file_ord: usize, node: &'a ViewNode, virt: u32, meta: FileMeta) {
        if !(self.kind)(node.kind) {
            return;
        }
        self.same_file
            .entry((file_ord, node.name.as_str()))
            .or_default()
            .push(virt);
        self.anywhere
            .entry(node.name.as_str())
            .or_default()
            .push((virt, meta));
    }
}

/// Mirror of index-time Pass-2 `Calls` resolution, narrowed to the inputs
/// available at query time, over the kind family of `names`. Returns the
/// merged-space target index.
///
/// Tier 1 — same file: the dirty file was FULLY re-parsed, so its own
/// callable set is authoritative. Unique match → confidence 1.0.
/// Tier 2 — import-scoped: callee name appears as an import's name/alias;
/// candidates narrowed to files matching the import source's last path
/// segment. Unique → 0.95.
/// Tier 3 — global: all clean-base callables (via the archived `name_index`)
/// plus all overlay callables, through the index-time candidate filter
/// [`pick_global`] (language and vendor barriers, unique only). ≥2
/// remaining → suppressed, matching `DecisionTier::AmbiguousGlobal` (an
/// invented edge is worse than a missing one). Unique → 0.7.
#[allow(clippy::too_many_arguments)]
fn resolve_callee(
    graph: &ArchivedZeroCopyGraph,
    callee: &str,
    file_ord: usize,
    file: &OverlayFileInput,
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    names: &OverlayNames<'_>,
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
) -> Option<(u32, f32)> {
    // Tier 1: same-file.
    if let Some(virts) = names.same_file.get(&(file_ord, callee)) {
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
            (names.kind)(NodeKind::from(&graph.nodes[idx as usize].kind))
                && !dirty_base.contains(&idx)
        })
        .collect();
    let overlay_candidates: &[(u32, FileMeta)] =
        names.anywhere.get(callee).map(Vec::as_slice).unwrap_or(&[]);

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
                // `base_candidates` excludes every dirty-base node, so a
                // scoped match can never need a replaced-base redirect —
                // surviving dirty symbols compete as overlay candidates with
                // their virtual index instead.
                debug_assert!(!replaced.contains_key(&scoped[0]));
                return Some((scoped[0], CONF_IMPORT_SCOPED));
            }
        }
    }

    // Tier 3: the shared candidate filter over clean base + overlay. Same
    // invariant as Tier 2: a base candidate is clean by construction, never
    // redirected; overlay candidates are virtual indices.
    let base = base_candidates.iter().map(|&idx| {
        let file_idx = graph.nodes[idx as usize].file_idx.to_native() as usize;
        let meta = *base_metas.entry(file_idx).or_insert_with(|| {
            graph
                .files
                .get(file_idx)
                .map_or_else(FileMeta::default, |f| {
                    FileMeta::from_path(f.path.resolve(&graph.string_pool))
                })
        });
        (idx, meta)
    });
    let GlobalPick::Unique(target) =
        pick_global(caller, base.chain(overlay_candidates.iter().copied()))
    else {
        return None;
    };
    debug_assert!(!replaced.contains_key(&target));
    Some((target, CONF_GLOBAL_UNIQUE))
}

/// Mirror of the index-time constructor fallback (`Resolver::resolve_call`):
/// a site the callable tiers left empty lands on the one constructor of the
/// Class / Struct it constructs, or on the type itself. Narrowed like
/// [`resolve_callee`] to bare names: a qualified type path resolves by its
/// last segment, and only when the site constructs for certain.
///
/// Fidelity gap: a clean type's constructors come from its base `HasMethod`
/// edges, which skip a constructor named like its class and, with two
/// classes in one file, all point at the file's first same-named
/// constructor (see `class_membership`).
#[allow(clippy::too_many_arguments)]
fn resolve_instantiation(
    graph: &ArchivedZeroCopyGraph,
    site: CallSite<'_>,
    file_ord: usize,
    file: &OverlayFileInput,
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    types: &OverlayNames<'_>,
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
) -> Option<(u32, f32)> {
    let (type_path, certain) = site.constructed_type(caller.language)?;
    let type_name = type_path
        .rsplit(['.', ':', '\\'])
        .next()
        .unwrap_or(type_path);
    if type_name.len() < type_path.len() && !certain {
        return None;
    }
    let (ty, confidence) = resolve_callee(
        graph, type_name, file_ord, file, caller, base_metas, types, replaced, dirty_base,
    )?;
    let base_len = graph.nodes.len() as u32;
    let ctors: Vec<u32> = match ty.checked_sub(base_len) {
        Some(virt_off) => {
            let ty_node = &nodes[virt_off as usize];
            if !ty_node.kind.is_constructible() {
                return None;
            }
            let mut ctors: Vec<u32> = nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    n.kind == NodeKind::Constructor
                        && Arc::ptr_eq(&n.rel_path, &ty_node.rel_path)
                        && owned_by(n.owner_class.as_deref(), type_name)
                })
                .map(|(i, _)| base_len + i as u32)
                .collect();
            // Overloads share a uid, and the index collapses them into the
            // first one: count them as that one constructor.
            ctors.dedup_by_key(|c| nodes[(*c - base_len) as usize].uid);
            ctors
        }
        None => {
            if !NodeKind::from(&graph.nodes[ty as usize].kind).is_constructible() {
                return None;
            }
            let start = graph.out_offsets[ty as usize].to_native() as usize;
            let end = graph.out_offsets[ty as usize + 1].to_native() as usize;
            graph.edges.as_slice()[start..end]
                .iter()
                .filter(|e| RelType::from(&e.rel_type) == RelType::HasMethod)
                .map(|e| e.target.to_native())
                .filter(|&t| NodeKind::from(&graph.nodes[t as usize].kind) == NodeKind::Constructor)
                .filter_map(|t| {
                    if dirty_base.contains(&t) {
                        replaced.get(&t).copied()
                    } else {
                        Some(t)
                    }
                })
                .collect()
        }
    };
    let target = match ctors.as_slice() {
        [ctor] => *ctor,
        _ => ty,
    };
    Some((target, confidence))
}

/// Does `owner_class` name type `ty`? Compares its last path segment with
/// generic arguments cut, like the index's owner key.
pub fn owned_by(owner_class: Option<&str>, ty: &str) -> bool {
    owner_class.is_some_and(|owner| {
        let owner = owner.split_once('<').map_or(owner, |(o, _)| o);
        owner.rsplit(['.', ':', '\\']).next().map(str::trim) == Some(ty)
    })
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
                    || std::path::Path::new(s)
                        .file_stem()
                        .and_then(|st| st.to_str())
                        .map(|st| st == segment)
                        .unwrap_or(false)
            }
            None => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::types::RawImport;
    use crate::graph_fixture::GraphFixture;
    use rkyv::rancor::Error as RkyvError;

    /// Fixture: a base graph with a real `name_index` and uid-correct nodes.
    ///
    /// files:  0 = src/clean.rs · 1 = src/dirty.rs · 2 = src/other.rs
    /// nodes:  0 keep_fn(dirty) · 1 gone_fn(dirty) · 2 callee_unique(clean)
    ///         3 dup_fn(clean) · 4 dup_fn(other) · 5 imp_fn(other)
    ///         6 imp_fn(clean)
    fn base_graph_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        // Pre-register in this order so `file_idx` matches the doc comment
        // above regardless of which file a node references first.
        let paths = ["src/clean.rs", "src/dirty.rs", "src/other.rs"];
        for p in paths {
            fx.file(p);
        }

        let specs: &[(&str, usize)] = &[
            ("keep_fn", 1),
            ("gone_fn", 1),
            ("callee_unique", 0),
            ("dup_fn", 0),
            ("dup_fn", 2),
            ("imp_fn", 2),
            ("imp_fn", 0),
        ];
        for (name, file_idx) in specs {
            fx.func(paths[*file_idx], name);
        }

        fx.into_bytes()
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
            .map(|(_, e)| e)
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
        // both dirty-file base nodes mask their stale outgoing Calls edges —
        // the overlay rebuilds Calls — but keep rels the fragment carries no
        // inputs for (mask ⊆ rebuild).
        assert!(view.masks_base_edge(0, RelType::Calls));
        assert!(view.masks_base_edge(1, RelType::Calls));
        assert!(!view.masks_base_edge(0, RelType::ReadsField));
        // clean nodes keep their base edges.
        assert!(!view.masks_base_edge(2, RelType::Calls));
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
        assert!(view.overlay_in(2).any(|(_, e)| e.source == keep_virt));
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
            .any(|(_, e)| e.source == caller_two_virt));
    }

    /// files: 0 = src/service.py · 1 = tests/fakes.py (Test) · 2 = src/helper.go
    /// nodes: 0 scan_range(service) · 1 scan_range(fakes) · 2 go_only(helper)
    fn barrier_graph_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        fx.file("src/service.py");
        fx.file_as("tests/fakes.py", crate::graph::FileCategory::Test);
        fx.file("src/helper.go");
        fx.func("src/service.py", "scan_range");
        fx.func("tests/fakes.py", "scan_range");
        fx.func("src/helper.go", "go_only");
        fx.into_bytes()
    }

    fn search_input(rel_path: &str) -> OverlayFileInput {
        OverlayFileInput {
            rel_path: rel_path.to_string(),
            symbols: vec![sym("search", &["scan_range", "go_only"])],
            imports: vec![],
        }
    }

    #[test]
    fn test_tier3_overlay_test_double_keeps_name_ambiguous_like_index() {
        let bytes = barrier_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        for caller in ["src/search.py", "tests/test_search.py"] {
            let view = OverlayView::build(graph, &[search_input(caller)]).unwrap();
            let search = view.base_len();
            assert!(edge_to(&view, search, 0).is_none(), "{caller}");
            assert!(edge_to(&view, search, 1).is_none(), "{caller}");
        }
    }

    #[test]
    fn test_tier3_overlay_language_barrier_matches_index() {
        // The index never resolves a Python call to a Go function; the
        // overlay used to, because its Tier 3 checked only the node kind.
        let bytes = barrier_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[search_input("src/search.py")]).unwrap();
        assert!(edge_to(&view, view.base_len(), 2).is_none());
    }

    /// files: 0 = src/widget.py · 1 = src/gadget.py
    /// nodes: 0 Widget(class) · 1 __init__(ctor of Widget) · 2 Gadget(class)
    fn construction_graph_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        fx.file("src/widget.py");
        fx.file("src/gadget.py");
        let widget = fx.node(NodeKind::Class, "src/widget.py", "Widget");
        let init = fx.node_owned(NodeKind::Constructor, "src/widget.py", "Widget", "__init__");
        fx.node(NodeKind::Class, "src/gadget.py", "Gadget");
        fx.edge(widget, init, RelType::HasMethod);
        fx.into_bytes()
    }

    #[test]
    fn test_build_unresolved_construction_calls_constructor_like_index() {
        let bytes = construction_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let untyped_member = format!("{}Widget", CallSite::UNTYPED_MEMBER_PREFIX);
        let app = OverlayFileInput {
            rel_path: "src/app.py".to_string(),
            symbols: vec![
                sym("make", &["Widget", "Gadget"]),
                sym("use_factory", &[untyped_member.as_str()]),
            ],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[app]).unwrap();
        let make = view.base_len();
        let use_factory = view.base_len() + 1;

        // `Widget()` lands on its one constructor, `Gadget()` on the class
        // that declares none: the edges the index builds for the same file.
        let e = edge_to(&view, make, 1).expect("make → Widget.__init__");
        assert_eq!(e.confidence, 0.7);
        assert!(edge_to(&view, make, 0).is_none(), "one edge per call site");
        assert!(edge_to(&view, make, 2).is_some(), "make → Gadget");
        // `obj.Widget()` on an untyped receiver is a method call.
        assert_eq!(view.overlay_out(use_factory).count(), 0);
    }

    /// The index collapses same-uid constructor overloads into the first
    /// one and lands there; a dirty file's overloads stay separate virtual
    /// nodes, so the overlay must count them as that one constructor too.
    #[test]
    fn test_build_construction_of_overloaded_dirty_type_calls_first_constructor_like_index() {
        let bytes = construction_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let ctor = |line: u32| OverlaySymbol {
            name: "Multi".to_string(),
            kind: NodeKind::Constructor,
            owner_class: Some("Multi".to_string()),
            start_line: line,
            end_line: line + 1,
            calls: vec![],
        };
        let multi = OverlayFileInput {
            rel_path: "src/Multi.java".to_string(),
            symbols: vec![
                OverlaySymbol {
                    kind: NodeKind::Class,
                    owner_class: None,
                    end_line: 9,
                    ..ctor(1)
                },
                ctor(2),
                ctor(5),
            ],
            imports: vec![],
        };
        let app = OverlayFileInput {
            rel_path: "src/App.java".to_string(),
            symbols: vec![sym("makeMulti", &["Multi"])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[multi, app]).unwrap();
        let first_ctor = view.base_len() + 1;
        let make = view.base_len() + 3;

        assert!(
            edge_to(&view, make, first_ctor).is_some(),
            "makeMulti → the first Multi overload"
        );
        assert_eq!(view.overlay_out(make).count(), 1, "one edge per call site");
    }
}
