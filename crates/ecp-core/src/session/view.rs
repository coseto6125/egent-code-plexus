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
//!   [`CallSite`]s, mirroring index-time Pass-2 tier semantics (same-file →
//!   import-scoped → unique-global with `AmbiguousGlobal` suppression, then
//!   the constructor fallback onto a Class / Struct's one constructor or the
//!   type) against the archived `name_index` (O(log N) per lookup, no
//!   allocation proportional to the graph).
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
//! - An import alias (`import { Widget as W }`) matches only by its local
//!   name, so a dirty file's call or construction through it stays
//!   unresolved; the index maps the alias back to the declared symbol.
//! - Rust paths resolve by conventional file layout and module declarations.
//!   `pub use` chains, `#[path]`, inline modules, and workspace crate-name
//!   heads stay unresolved when that layout cannot establish the target.

use crate::analyzer::rust_paths::{
    is_rust_source, is_rust_target_root, rust_module_dir, rust_module_path_base,
};
use crate::analyzer::types::{owner_key, CallSite, RawImport};
use crate::file_category::{pick_global, FileMeta, GlobalPick, Language};
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
    /// `RawNode.calls` of this symbol: callee short names, some encoded as a
    /// [`CallSite`] (read them through [`CallSite::parse`]).
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
    /// Virtual Class / Struct idx → the virtual constructors of its file
    /// whose [`owner_key`] is its name, in source order. Types with none have
    /// no entry.
    type_ctors: FxHashMap<u32, Vec<u32>>,
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

        let type_ctors = virtual_constructors(files, &nodes, base_len);

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
            // Constructed type → its `Calls` target: a type is often
            // constructed at several sites, and a clean one costs name-index
            // probes.
            let mut construction_targets: FxHashMap<u32, u32> = FxHashMap::default();
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
            let rust_modules = files
                .iter()
                .any(|file| {
                    is_rust_source(std::path::Path::new(&file.rel_path))
                        && file
                            .symbols
                            .iter()
                            .any(|s| s.calls.iter().any(|c| c.contains("::")))
                })
                .then(|| RustModules::new(graph, files));
            for (file_ord, file) in files.iter().enumerate() {
                let chain = rust_modules
                    .as_ref()
                    .and_then(|modules| modules.caller_chain(&file.rel_path));
                // One cache per dirty file: a repeated qualified call reuses
                // both successful and unresolved lookups without rebuilding paths.
                let mut module_calls = FxHashMap::default();
                for sym in &file.symbols {
                    let source = virt_cursor;
                    virt_cursor += 1;
                    for raw_callee in &sym.calls {
                        let site = CallSite::parse(raw_callee);
                        let caller = file_metas[file_ord];
                        let hit =
                            if caller.language == Language::Rust && site.name().contains("::") {
                                *module_calls.entry(site.name()).or_insert_with(|| {
                                    resolve_rust_module_callee(
                                        graph,
                                        site.name(),
                                        file,
                                        &callables,
                                        &nodes,
                                        &dirty_base,
                                        rust_modules.as_ref()?,
                                        chain.as_deref()?,
                                    )
                                })
                            } else {
                                resolve_callee(
                                    graph,
                                    site,
                                    file_ord,
                                    file,
                                    caller,
                                    &mut base_metas,
                                    &callables,
                                    &nodes,
                                    &replaced,
                                    &dirty_base,
                                )
                            }
                            .or_else(|| {
                                let (ty, confidence) = resolve_constructed_type(
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
                                )?;
                                let target = *construction_targets.entry(ty).or_insert_with(|| {
                                    construction_target(graph, &nodes, &type_ctors, ty)
                                });
                                Some((target, confidence))
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
            type_ctors,
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

    /// Constructors of the virtual Class / Struct `idx`: the virtual
    /// Constructor nodes of its file whose [`owner_key`] is its name, in
    /// source order, overloads included. Empty for any other node.
    pub fn constructors_of(&self, idx: u32) -> &[u32] {
        self.type_ctors.get(&idx).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// [`OverlayView::constructors_of`] for every virtual Class / Struct.
fn virtual_constructors(
    files: &[OverlayFileInput],
    nodes: &[ViewNode],
    base_len: u32,
) -> FxHashMap<u32, Vec<u32>> {
    let mut by_owner: FxHashMap<(usize, &str), Vec<u32>> = FxHashMap::default();
    let mut types: Vec<(usize, u32)> = Vec::new();
    let mut virt = base_len;
    for (file_ord, file) in files.iter().enumerate() {
        for _ in &file.symbols {
            let node = &nodes[(virt - base_len) as usize];
            if node.kind == NodeKind::Constructor {
                if let Some(owner) = node.owner_class.as_deref().and_then(owner_key) {
                    by_owner.entry((file_ord, owner)).or_default().push(virt);
                }
            } else if node.kind.is_constructible() {
                types.push((file_ord, virt));
            }
            virt += 1;
        }
    }
    types
        .into_iter()
        .filter_map(|(file_ord, ty)| {
            let name = nodes[(ty - base_len) as usize].name.as_str();
            by_owner
                .get(&(file_ord, name))
                .map(|ctors| (ty, ctors.clone()))
        })
        .collect()
}

/// Constructors of the base Class / Struct `type_idx`: the Constructor
/// nodes in its file whose [`owner_key`] is its name. Empty for any other
/// kind. The index's `SymbolTable::build_constructor_index` applies the
/// same rule.
///
/// `HasMethod` cannot list them: `class_membership` skips a constructor
/// named like its class (Java, C#, Kotlin, C++, Dart), and with two classes
/// in one file binds each `__init__` / `constructor` to the first. So its
/// edges only supply candidate names: the type's own name plus each
/// constructor name they reach, each probed once in the name index.
pub fn base_constructors(graph: &ArchivedZeroCopyGraph, type_idx: u32) -> Vec<u32> {
    let pool = &graph.string_pool;
    let ty = &graph.nodes[type_idx as usize];
    if !NodeKind::from(&ty.kind).is_constructible() || !ty.has_owning_file() {
        return Vec::new();
    }
    let name = ty.name.resolve(pool);
    let start = graph.out_offsets[type_idx as usize].to_native() as usize;
    let end = graph.out_offsets[type_idx as usize + 1].to_native() as usize;
    let mut probes: Vec<&str> = vec![name];
    for edge in &graph.edges.as_slice()[start..end] {
        let member = &graph.nodes[edge.target.to_native() as usize];
        if RelType::from(&edge.rel_type) == RelType::HasMethod
            && NodeKind::from(&member.kind) == NodeKind::Constructor
        {
            let member_name = member.name.resolve(pool);
            if !probes.contains(&member_name) {
                probes.push(member_name);
            }
        }
    }
    probes
        .iter()
        .flat_map(|&probe| graph.nodes_by_name(probe))
        .filter(|&idx| {
            let node = &graph.nodes[idx as usize];
            NodeKind::from(&node.kind) == NodeKind::Constructor
                && node.file_idx == ty.file_idx
                && owner_key(node.owner_class.resolve(pool)) == Some(name)
        })
        .collect()
}

/// [`base_constructors`] for many types: every base Constructor grouped by
/// its file and [`owner_key`], built in one pass over the nodes. Every
/// `constructor` / `__init__` shares one name, so probing the name index
/// once per type costs types x constructors on a large repo (vscode: a
/// fuzzy `find` over 12k classes against 10.7k `constructor` nodes).
pub struct BaseConstructorIndex<'g> {
    by_owner: FxHashMap<(u32, &'g str), Vec<u32>>,
}

impl<'g> BaseConstructorIndex<'g> {
    pub fn build(graph: &'g ArchivedZeroCopyGraph) -> Self {
        let pool = &graph.string_pool;
        let mut by_owner: FxHashMap<(u32, &'g str), Vec<u32>> = FxHashMap::default();
        for (idx, node) in graph.nodes.iter().enumerate() {
            if NodeKind::from(&node.kind) != NodeKind::Constructor {
                continue;
            }
            if let Some(owner) = owner_key(node.owner_class.resolve(pool)) {
                by_owner
                    .entry((node.file_idx.to_native(), owner))
                    .or_default()
                    .push(idx as u32);
            }
        }
        Self { by_owner }
    }

    /// The same set [`base_constructors`] returns for `type_idx`.
    pub fn of(&self, graph: &'g ArchivedZeroCopyGraph, type_idx: u32) -> &[u32] {
        let ty = &graph.nodes[type_idx as usize];
        if !NodeKind::from(&ty.kind).is_constructible() || !ty.has_owning_file() {
            return &[];
        }
        let key = (ty.file_idx.to_native(), ty.name.resolve(&graph.string_pool));
        self.by_owner.get(&key).map_or(&[], Vec::as_slice)
    }
}

/// The `Calls` target of a construction of the Class / Struct `ty`: its one
/// constructor, else the type itself.
fn construction_target(
    graph: &ArchivedZeroCopyGraph,
    nodes: &[ViewNode],
    type_ctors: &FxHashMap<u32, Vec<u32>>,
    ty: u32,
) -> u32 {
    let base_len = graph.nodes.len() as u32;
    let uid = |idx: u32| match idx.checked_sub(base_len) {
        Some(virt_off) => nodes[virt_off as usize].uid,
        None => graph.nodes[idx as usize].uid.to_native(),
    };
    let base;
    let ctors: &[u32] = if ty >= base_len {
        type_ctors.get(&ty).map(Vec::as_slice).unwrap_or(&[])
    } else {
        base = base_constructors(graph, ty);
        &base
    };
    sole_constructor(ctors, uid).unwrap_or(ty)
}

/// The one constructor among `ctors`, or `None`. A dirty file's overloads
/// are separate virtual nodes sharing one uid, which the index collapses
/// into the first: constructors that all share the first one's uid count as
/// that one, whatever their order.
fn sole_constructor(ctors: &[u32], uid: impl Fn(u32) -> u64) -> Option<u32> {
    let (&first, rest) = ctors.split_first()?;
    let first_uid = uid(first);
    rest.iter().all(|&c| uid(c) == first_uid).then_some(first)
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
/// Rust `::` calls use the cached module-path branch in `build` first.
/// Method eligibility is a post-filter, like index-time `lookup_call_in_file`:
/// removing free functions before uniqueness checks would invent a winner.
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
    site: CallSite<'_>,
    file_ord: usize,
    file: &OverlayFileInput,
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    names: &OverlayNames<'_>,
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
) -> Option<(u32, f32)> {
    let callee = site.name();
    let accepts = |idx: u32| {
        !(site.uses_method_syntax() && caller.language == Language::Rust)
            || match idx.checked_sub(graph.nodes.len() as u32) {
                Some(offset) => nodes[offset as usize].kind == NodeKind::Method,
                None => NodeKind::from(&graph.nodes[idx as usize].kind) == NodeKind::Method,
            }
    };
    // Tier 1: same-file.
    if let Some(virts) = names.same_file.get(&(file_ord, callee)) {
        if virts.len() == 1 {
            return accepts(virts[0]).then_some((virts[0], CONF_SAME_FILE));
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
                return accepts(scoped[0]).then_some((scoped[0], CONF_IMPORT_SCOPED));
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
    accepts(target).then_some((target, CONF_GLOBAL_UNIQUE))
}

/// File-backed Rust modules use the same crate/self/super layout as Pass 2.
/// Unknown heads stay unresolved: stripping a path would bind external
/// calls such as `std::fs::read` to unrelated project functions.
/// File-layout evidence shared by all qualified lookups in one overlay build.
/// Dirty declarations replace archived declarations, including deletion.
struct RustModules<'a> {
    graph: &'a ArchivedZeroCopyGraph,
    dirty: FxHashMap<&'a str, &'a OverlayFileInput>,
    roots: Vec<&'a str>,
}

impl<'a> RustModules<'a> {
    fn new(graph: &'a ArchivedZeroCopyGraph, dirty: &'a [OverlayFileInput]) -> Self {
        let dirty: FxHashMap<_, _> = dirty
            .iter()
            .map(|file| (file.rel_path.as_str(), file))
            .collect();
        let mut roots = Vec::new();
        for file in graph
            .files
            .iter()
            .map(|f| f.path.resolve(&graph.string_pool))
            .chain(dirty.keys().copied())
        {
            let directory = file.rsplit_once('/').map_or("", |(dir, _)| dir);
            if !dirty.keys().any(|caller| {
                caller
                    .strip_prefix(directory)
                    .is_some_and(|rest| directory.is_empty() || rest.starts_with('/'))
            }) {
                continue;
            }
            let mut parts = file.rsplit('/');
            let filename = parts.next().unwrap_or_default();
            let parent = parts.next().unwrap_or_default();
            if (matches!(filename, "lib.rs" | "main.rs" | "build.rs")
                || matches!(parent, "bin" | "examples" | "benches" | "tests"))
                && is_rust_target_root(std::path::Path::new(file))
            {
                roots.push(file);
            }
        }
        roots.sort_unstable();
        roots.dedup();
        Self {
            graph,
            dirty,
            roots,
        }
    }

    fn file(&self, path: &std::path::Path) -> Option<&'a str> {
        let requested = path.to_str()?;
        if let Some((&name, _)) = self.dirty.get_key_value(requested) {
            return Some(name);
        }
        let name =
            |file: &'a crate::graph::ArchivedFile| file.path.resolve(&self.graph.string_pool);
        // Accept only an exact hit in the builder's usual Path order.
        // Synthetic/older archives with another order use the safe scan.
        if let Ok(idx) = self
            .graph
            .files
            .binary_search_by(|file| std::path::Path::new(name(file)).cmp(path))
        {
            let found = name(&self.graph.files[idx]);
            if found == requested {
                return Some(found);
            }
        }
        self.graph
            .files
            .iter()
            .map(name)
            .find(|file| *file == requested)
    }

    // Match the full resolver's qualifier file-stem scope bucket.
    fn scope(path: &str) -> &str {
        path.rsplit_once("/src/")
            .or_else(|| path.rsplit_once("/tests/"))
            .map_or("", |(root, _)| root)
    }

    fn child(&self, parent: &str, name: &str) -> Option<&'a str> {
        let declared = match self.dirty.get(parent) {
            Some(dirty) => dirty
                .symbols
                .iter()
                .any(|s| s.kind == NodeKind::Module && s.name == name),
            None => self.graph.nodes_by_name(name).any(|idx| {
                let node = &self.graph.nodes[idx as usize];
                NodeKind::from(&node.kind) == NodeKind::Module
                    && self.graph.files[node.file_idx.to_native() as usize]
                        .path
                        .resolve(&self.graph.string_pool)
                        == parent
            }),
        };
        if !declared {
            return None;
        }
        let base = rust_module_path_base(std::path::Path::new(parent), "self")?.join(name);
        let flat = base.with_extension("rs");
        let nested = base.join("mod.rs");
        match (self.file(&flat), self.file(&nested)) {
            (Some(path), None) | (None, Some(path)) => Some(path),
            _ => None,
        }
    }

    /// Establish the caller's unique target owner using declaration chains.
    /// A bin descendant only belongs to a bin when its target root is indexed.
    fn caller_chain(&self, caller: &str) -> Option<Vec<&'a str>> {
        let mut found = None;
        for &root in &self.roots {
            let mut chain = vec![root];
            if root != caller {
                let root_dir = rust_module_dir(std::path::Path::new(root))?;
                let Ok(relative) = std::path::Path::new(caller).strip_prefix(&root_dir) else {
                    continue;
                };
                let mut module_path = relative.with_extension("");
                if module_path.file_name().is_some_and(|name| name == "mod") {
                    module_path.pop();
                }
                for part in module_path.components() {
                    let Some(child) =
                        self.child(chain.last().copied()?, part.as_os_str().to_str()?)
                    else {
                        break;
                    };
                    chain.push(child);
                }
                if chain.last().copied() != Some(caller) {
                    continue;
                }
            }
            if found.is_some() {
                return None;
            }
            found = Some(chain);
        }
        found
    }

    fn resolve(&self, chain: &[&'a str], module: &str) -> Option<&'a str> {
        let mut parts = module.split("::").peekable();
        let head = parts.next()?;
        let mut at = chain.len().checked_sub(1)?;
        let mut file = match head {
            "crate" => *chain.first()?,
            "self" => chain[at],
            "super" => {
                at = at.checked_sub(1)?;
                while parts.peek() == Some(&"super") {
                    parts.next();
                    at = at.checked_sub(1)?;
                }
                chain[at]
            }
            _ if !module.contains("::") => {
                let child = self.child(chain[at], head)?;
                // The full resolver handles simple declared heads through its
                // file-stem tier; nested unanchored paths remain unresolved.
                return (std::path::Path::new(child).file_stem()?.to_str()? == head)
                    .then_some(child);
            }
            _ => return None,
        };
        for part in parts {
            file = self.child(file, part)?;
        }
        Some(file)
    }

    fn confidence(&self, caller: &str, module: &str, target: &str) -> f32 {
        const CONF_QUALIFIER_SCOPED: f32 = 0.85;
        const CONF_MODULE_TREE: f32 = 1.0;
        let (prefix, qualifier) = module.rsplit_once("::").unwrap_or(("", module));
        let internal = prefix.is_empty()
            || prefix
                .split("::")
                .all(|s| matches!(s, "crate" | "self" | "super"));
        let qualifier_file = |file: &&str| {
            file.rsplit('/')
                .next()
                .and_then(|name| name.strip_suffix(".rs"))
                == Some(qualifier)
                && Self::scope(file) == Self::scope(caller)
        };
        let mut matches = self
            .graph
            .files
            .iter()
            .map(|f| f.path.resolve(&self.graph.string_pool))
            .filter(qualifier_file)
            .filter(|file| !self.dirty.contains_key(file))
            .chain(self.dirty.keys().copied())
            .filter(qualifier_file);
        if internal && matches.next() == Some(target) && matches.next().is_none() {
            CONF_QUALIFIER_SCOPED
        } else {
            CONF_MODULE_TREE
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_rust_module_callee(
    graph: &ArchivedZeroCopyGraph,
    callee: &str,
    file: &OverlayFileInput,
    names: &OverlayNames<'_>,
    nodes: &[ViewNode],
    dirty_base: &FxHashSet<u32>,
    modules: &RustModules<'_>,
    chain: &[&str],
) -> Option<(u32, f32)> {
    let (module, member) = callee.rsplit_once("::")?;
    let target_file = modules.resolve(chain, module)?;
    let base = graph.nodes_by_name(member).filter(|&idx| {
        let node = &graph.nodes[idx as usize];
        !dirty_base.contains(&idx)
            && (names.kind)(NodeKind::from(&node.kind))
            && NodeKind::from(&node.kind) != NodeKind::Method
            && graph.files[node.file_idx.to_native() as usize]
                .path
                .resolve(&graph.string_pool)
                == target_file
    });
    let overlay = names
        .anywhere
        .get(member)
        .into_iter()
        .flatten()
        .filter_map(|&(idx, _)| {
            let node = &nodes[(idx - graph.nodes.len() as u32) as usize];
            (node.rel_path.as_ref() == target_file && node.kind != NodeKind::Method).then_some(idx)
        });
    let mut candidates = base.chain(overlay);
    let target = candidates.next()?;
    let uid = |idx: u32| match idx.checked_sub(graph.nodes.len() as u32) {
        Some(offset) => nodes[offset as usize].uid,
        None => graph.nodes[idx as usize].uid.to_native(),
    };
    candidates.all(|other| uid(other) == uid(target)).then(|| {
        (
            target,
            modules.confidence(&file.rel_path, module, target_file),
        )
    })
}

/// Mirror of the index-time constructor fallback (`Resolver::resolve_call`)
/// up to the constructed type: a site the callable tiers left empty resolves
/// the Class / Struct it constructs; [`construction_target`] then picks the
/// edge's target. Narrowed like [`resolve_callee`] to bare names: a
/// qualified type path resolves by its last segment, and only where
/// [`CallSite::constructed_type`] allows that fallback.
#[allow(clippy::too_many_arguments)]
fn resolve_constructed_type(
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
    let (type_path, last_segment_fallback) = site.constructed_type(caller.language)?;
    let type_name = type_path
        .rsplit(['.', ':', '\\'])
        .next()
        .unwrap_or(type_path);
    if type_name.len() < type_path.len() && !last_segment_fallback {
        return None;
    }
    // Most unresolved calls name no type at all: reject them before the
    // candidate collection in `resolve_callee` allocates.
    let names_constructible = types.anywhere.contains_key(type_name)
        || graph
            .nodes_by_name(type_name)
            .any(|idx| NodeKind::from(&graph.nodes[idx as usize].kind).is_constructible());
    if !names_constructible {
        return None;
    }
    let (ty, confidence) = resolve_callee(
        graph,
        CallSite::Plain(type_name),
        file_ord,
        file,
        caller,
        base_metas,
        types,
        nodes,
        replaced,
        dirty_base,
    )?;
    let kind = match ty.checked_sub(graph.nodes.len() as u32) {
        Some(virt_off) => nodes[virt_off as usize].kind,
        None => NodeKind::from(&graph.nodes[ty as usize].kind),
    };
    kind.is_constructible().then_some((ty, confidence))
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
    fn test_rust_module_file_lookup_unordered_archive_finds_exact_paths() {
        let mut fixture = GraphFixture::new();
        let paths = ["src/z.rs", "src/lib.rs", "src/a/mod.rs", "src/b.rs"];
        for path in paths {
            fixture.file(path);
        }
        let bytes = fixture.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let modules = RustModules::new(graph, &[]);
        for path in paths {
            assert_eq!(modules.file(std::path::Path::new(path)), Some(path));
        }
        assert_eq!(modules.file(std::path::Path::new("src/missing.rs")), None);
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
        let untyped_member = CallSite::untyped_member("Widget");
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
        // The Java extractor encodes `new Multi(1)` as a construction.
        let new_multi = CallSite::construct("Multi");
        let app = OverlayFileInput {
            rel_path: "src/App.java".to_string(),
            symbols: vec![sym("makeMulti", &[new_multi.as_str()])],
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

    /// files: 0 = src/models.py
    /// nodes: 0 Alpha · 1 Alpha.__init__ · 2 Beta · 3 Beta.__init__
    ///
    /// `class_membership` binds both classes' `HasMethod` to the file's
    /// first `__init__`; the fixture keeps that defect.
    fn two_class_graph_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        fx.file("src/models.py");
        let alpha = fx.node(NodeKind::Class, "src/models.py", "Alpha");
        let alpha_init = fx.node_owned(NodeKind::Constructor, "src/models.py", "Alpha", "__init__");
        let beta = fx.node(NodeKind::Class, "src/models.py", "Beta");
        fx.node_owned(NodeKind::Constructor, "src/models.py", "Beta", "__init__");
        fx.edge(alpha, alpha_init, RelType::HasMethod);
        fx.edge(beta, alpha_init, RelType::HasMethod);
        fx.into_bytes()
    }

    /// The index picks the constructor its owner names, never the one a
    /// `HasMethod` edge points at.
    #[test]
    fn test_build_construction_of_second_class_in_clean_file_calls_its_own_constructor() {
        let bytes = two_class_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let app = OverlayFileInput {
            rel_path: "src/app.py".to_string(),
            symbols: vec![sym("make", &["Beta"])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[app]).unwrap();
        let make = view.base_len();

        assert!(edge_to(&view, make, 3).is_some(), "make → Beta.__init__");
        assert_eq!(view.overlay_out(make).count(), 1, "one edge per call site");
        let merged = crate::session::MergedGraph::new(graph, Some(&view));
        assert_eq!(merged.constructors_of(2), [3], "Beta's constructors");
        assert_eq!(merged.constructors_of(0), [1], "Alpha's constructors");
    }

    /// The overlay compares owners through the index's `owner_key`: a
    /// generic path (`Outer<T>.Inner`) names its last segment, `Inner`.
    #[test]
    fn test_build_construction_owner_with_generic_path_calls_constructor_like_index() {
        let bytes = construction_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let symbol = |name: &str, kind, owner: &str, line: u32| OverlaySymbol {
            name: name.to_string(),
            kind,
            owner_class: Some(owner.to_string()),
            start_line: line,
            end_line: line + 1,
            calls: vec![],
        };
        let outer = OverlayFileInput {
            rel_path: "src/outer.ts".to_string(),
            symbols: vec![
                symbol("Inner", NodeKind::Class, "Outer<T>", 2),
                symbol("constructor", NodeKind::Constructor, "Outer<T>.Inner", 3),
            ],
            imports: vec![],
        };
        let new_inner = CallSite::construct("Inner");
        let app = OverlayFileInput {
            rel_path: "src/app.ts".to_string(),
            symbols: vec![sym("makeInner", &[new_inner.as_str()])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[outer, app]).unwrap();
        let ctor = view.base_len() + 1;
        let make = view.base_len() + 2;

        assert!(
            edge_to(&view, make, ctor).is_some(),
            "makeInner → Inner's constructor"
        );
        assert_eq!(view.overlay_out(make).count(), 1, "one edge per call site");
        assert_eq!(view.constructors_of(view.base_len()), [ctor]);
    }

    /// Contract: overloads (one uid) count as their first node in any
    /// order; two distinct constructors are several.
    #[test]
    fn test_sole_constructor_overloads_and_distinct_returns_first_or_none() {
        let uid = |idx: u32| if idx == 7 { 2 } else { 1 };
        assert_eq!(sole_constructor(&[3, 5], uid), Some(3));
        assert_eq!(sole_constructor(&[3, 7, 5], uid), None);
        assert_eq!(sole_constructor(&[7], uid), Some(7));
        assert_eq!(sole_constructor(&[], uid), None);
    }
}
