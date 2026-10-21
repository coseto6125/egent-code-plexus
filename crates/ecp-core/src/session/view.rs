//! Query-time L1 overlay merge view (FU-2026-06-10-398a846bca42 root cure).
//!
//! `OverlayView` makes uncommitted-edit symbols AND edges visible to graph
//! traversals (impact, cypher) without rebuilding L2 and without loading
//! O(graph) data. The base `ArchivedZeroCopyGraph` stays an immutable mmap;
//! the view is a small in-memory delta whose own allocation is O(dirty
//! symbols). Building it scans `graph.files` and `graph.nodes` once each per
//! query; two lazy helpers cost more, and only when their call shapes occur
//! ([`OverlayView::build`] has the full cost):
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
//!   report them — this is what kills phantom callers after a rename. Only
//!   kinds a fragment can carry are suppressed; nodes the graph builder
//!   makes (File, Route, EntryPoint, Process, PathLiteral, detector nodes)
//!   stay visible as indexed, and their edges into dirty symbols redirect
//!   or drop like any other base edge.
//! - **overlay edges** — `Calls` edges re-resolved from each dirty symbol's
//!   [`CallSite`]s, mirroring index-time Pass-2 tier semantics (same-file
//!   with the caller's own member first → import-scoped → unique-global with
//!   `AmbiguousGlobal` suppression unless every candidate is a copy of one
//!   declaration in duplicated source trees, then the constructor fallback
//!   onto a Class / Struct's one constructor or the type) against the
//!   archived `name_index` (O(log N) per lookup, no allocation proportional
//!   to the graph). A nearest-copy edge keeps the index's
//!   [`NEAREST_COPY_REASON`]; every other overlay call carries the
//!   uncommitted-edit marker.
//! - **closure references** — lexical `References` from each enclosing
//!   callable to its anonymous closures, using the full index's span rule.
//!   The merge replaces only base references with `closure:lexical_reference`
//!   from dirty sources, preserving every other References reason.
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
//! - `References` edges other than lexical closure references are not
//!   re-resolved: a dirty file keeps the base ones of its surviving symbols
//!   (stale) and gains none for new code.
//! - Builder-made nodes of a dirty file and their edges are as indexed; a
//!   brand-new symbol gets no containment edge (`File -Defines->`, …).
//! - Clean files calling a name that only NOW resolves (new symbol breaks a
//!   previous ambiguity) keep their index-time resolution — clean files are
//!   never re-resolved at query time.
//! - TS / JS imports run the index's named-import tier through the shared
//!   rules ([`reexport`]): candidate probing, ES-module export eligibility,
//!   export aliases and the one-hop barrel re-export, over the archived
//!   import / export facts of clean files. `graph.bin` keeps no tsconfig
//!   path aliases, so an aliased specifier of a dirty importer that probes
//!   no file falls to the segment match below and follows no re-export. A
//!   clean barrel's re-export rows keep the file the index resolved them to,
//!   aliased specifiers included, so the hop through a clean barrel follows
//!   them; an aliased specifier inside a dirty barrel is not resolved.
//! - Outside the import-member languages and TS / JS, only a `./` / `../`
//!   import source probes the index's candidate files. Any other source (a
//!   package or `crate::` path) narrows by its last path segment, unique
//!   only, an approximation of the index's module tree; a namespace import
//!   through such a source binds nothing.
//! - The qualifier tier (`K.save`, `K::st`) is replayed except where the
//!   index walks the Rust module tree (a `use` of the qualifier, a path
//!   through an external module) or a tsconfig path alias, and its
//!   module-file tier counts a base file that declares nothing (the index
//!   skips it), which can only leave a call unresolved. The heritage tier and
//!   the receiver-typing ladder are not replayed: a qualified callee whose
//!   member the qualifier's file lacks (inherited, `super.setup`) stays
//!   unresolved, and a wildcard import binds a name that the index resolves
//!   on the caller's base class. The ladder is also where the index binds a
//!   qualified call into duplicated trees (`Maps.make()` with two copies of
//!   `Maps`) to the nearest copy; the overlay leaves that call unresolved.
//! - Within one file, the index binds the first of several same-name
//!   callables (a call from a member of `K` takes `K`'s own one). The overlay
//!   binds only `K`'s own one and leaves any other same-file ambiguity
//!   unresolved.
//! - Rust paths resolve by conventional file layout and module declarations.
//!   `pub use` chains, `#[path]`, inline modules, and workspace crate-name
//!   heads stay unresolved when that layout cannot establish the target.

use super::import_scope::{ImportScope, Module};
use crate::analyzer::import_binding::{
    extension_retry, fqn_language, import_binding, import_member_fallback, owner_path_agrees,
    retry_name, DeclaredType, MemberOwner,
};
use crate::analyzer::reexport::{
    self, export_filter_applies, is_ecmascript, ExportAliasTable, Fact, FileImports, ImportRef,
    ModuleFacts, PyModule, ResolveTarget,
};
use crate::analyzer::rust_paths::{
    is_rust_source, is_rust_target_root, rust_module_dir, rust_module_path_base,
};
use crate::analyzer::types::{
    innermost_enclosing, owner_key, CallSite, LocalGraph, RawExportAlias, RawImport,
    CLOSURE_REFERENCE_REASON, NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON,
};
use crate::file_category::{pick_global, FileMeta, GlobalPick, Language};
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
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
    /// 0-based columns from the fragment, preserving same-line containment.
    pub start_column: u32,
    pub end_column: u32,
    /// `RawNode.calls` of this symbol: callee short names, some encoded as a
    /// [`CallSite`] (read them through [`CallSite::parse`]).
    pub calls: Vec<String>,
    /// `RawNode.is_exported`: in an ES module, only an exported declaration
    /// is another file's import target.
    pub is_exported: bool,
}

/// One dirty file's fresh parse, as loaded from a fragment bin. Its imports
/// and exports replace the archived ones of the same file, empty included.
#[derive(Debug, Clone)]
pub struct OverlayFileInput {
    /// Repo-relative path, same normalization as `File.path` in the graph.
    pub rel_path: String,
    pub symbols: Vec<OverlaySymbol>,
    pub imports: Vec<RawImport>,
    /// `LocalGraph.export_aliases`, empty when the file has none.
    pub export_aliases: Vec<RawExportAlias>,
}

impl From<LocalGraph> for OverlayFileInput {
    fn from(local: LocalGraph) -> Self {
        Self {
            rel_path: local.file_path.to_string_lossy().replace('\\', "/"),
            symbols: local
                .nodes
                .into_iter()
                .map(|n| OverlaySymbol {
                    name: n.name,
                    kind: n.kind,
                    owner_class: n.owner_class,
                    start_line: n.span.0 + 1,
                    end_line: n.span.2 + 1,
                    start_column: n.span.1,
                    end_column: n.span.3,
                    calls: n.calls,
                    is_exported: n.is_exported,
                })
                .collect(),
            imports: local.imports,
            export_aliases: local.export_aliases.map(Vec::from).unwrap_or_default(),
        }
    }
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
    /// A later overload ([`crate::uid::keeps_overloads`]): the index keeps
    /// it as a node under its own uid but never binds it by name.
    pub later_overload: bool,
}

/// An overlay-resolved edge. Indices are merged-space: `< base_len` = base
/// node, `>= base_len` = virtual node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewEdge {
    pub source: u32,
    pub target: u32,
    pub rel_type: RelType,
    pub confidence: f32,
    pub reason: &'static str,
}

/// Tier confidences mirroring index-time Pass-2 resolution.
const CONF_SAME_FILE: f32 = 1.0;
const CONF_IMPORT_SCOPED: f32 = 0.95;
const CONF_QUALIFIER_SCOPED: f32 = 0.85;
const CONF_GLOBAL_UNIQUE: f32 = 0.7;

#[derive(Debug, Default)]
pub struct OverlayView {
    base_len: u32,
    nodes: Vec<ViewNode>,
    /// base idx → virtual idx for surviving (uid-matched) symbols.
    replaced: FxHashMap<u32, u32>,
    /// base idxs of dirty-file symbols NOT re-emitted (deleted/renamed).
    suppressed: FxHashSet<u32>,
    /// every base idx of a dirty file whose kind [`fragment_emits`]
    /// (replaced ∪ suppressed): base edges sourced here are masked in
    /// favour of overlay adjacency. Builder-made nodes (File, Route, …)
    /// stay outside it, visible and slightly stale.
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
    /// O(nodes) pass over `graph.nodes` (pure scans; the view's own
    /// allocation stays O(dirty symbols)), then O(dirty symbols × callees ×
    /// log N) name-index lookups. Two lazy helpers add more:
    /// - a Python/Java/Kotlin/PHP call through an import builds, once per
    ///   such language, a file index over `graph.files` that allocates
    ///   O(files of that language) for module discovery;
    /// - a dirty Rust file with a `::` call makes one more pass over
    ///   `graph.files` for crate roots, plus one per distinct resolved
    ///   qualified callee of each dirty file for the qualifier-scope
    ///   confidence;
    /// - an ambiguous Tier-3 call whose first two candidates are copies of
    ///   one Java / Kotlin / PHP declaration builds that language's file
    ///   index too, to compare their files' namespaces.
    ///
    /// The shared import rules scan nothing: a TS / JS import probes its
    /// candidate paths by binary search over `graph.files`, reads one
    /// module's archived import rows per re-export hop, and looks
    /// declarations up through the name index.
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
        // match too, see overlay_writer). Only kinds a fragment can re-emit
        // are collected: mask ⊆ rebuild applies to nodes too.
        let mut dirty_base_by_uid: FxHashMap<u64, u32> = FxHashMap::default();
        let mut dirty_base: FxHashSet<u32> = FxHashSet::default();
        if !dirty_file_idx.is_empty() {
            for (i, node) in graph.nodes.iter().enumerate() {
                if dirty_file_idx.contains(&node.file_idx.to_native())
                    && fragment_emits(NodeKind::from(&node.kind))
                {
                    dirty_base_by_uid.insert(node.uid.to_native(), i as u32);
                    dirty_base.insert(i as u32);
                }
            }
        }

        // ── virtual nodes + replaced mapping ──────────────────────────────
        let mut nodes: Vec<ViewNode> = Vec::new();
        let mut replaced: FxHashMap<u32, u32> = FxHashMap::default();
        // Identity uid → first virtual idx, and first → its later overloads,
        // as the index's `SymbolTable::register_overload` numbers them.
        let mut first_of: FxHashMap<u64, u32> = FxHashMap::default();
        let mut later_overloads: FxHashMap<u32, Vec<u32>> = FxHashMap::default();

        for file in files {
            let rel: Arc<str> = Arc::from(file.rel_path.as_str());
            for sym in &file.symbols {
                let identity = crate::uid::compute(
                    sym.kind,
                    &file.rel_path,
                    sym.owner_class.as_deref(),
                    &sym.name,
                );
                let virt = base_len + nodes.len() as u32;
                let first = *first_of.entry(identity).or_insert(virt);
                let later_overload = first != virt && crate::uid::keeps_overloads(sym.kind);
                let uid = if later_overload {
                    let later = later_overloads.entry(first).or_default();
                    later.push(virt);
                    crate::uid::overload(identity, later.len() as u32)
                } else {
                    identity
                };
                let replaced_base = dirty_base_by_uid.get(&uid).copied();
                // First UID wins, as in full indexing: later twins are
                // tombstones there, so base edges must reach the first one.
                // A later overload has a uid of its own and maps to its own
                // base node.
                if let Some(base_idx) = replaced_base {
                    replaced.entry(base_idx).or_insert(virt);
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
                    later_overload,
                });
            }
        }
        let suppressed: FxHashSet<u32> = dirty_base
            .iter()
            .copied()
            .filter(|idx| !replaced.contains_key(idx))
            .collect();

        let type_ctors = virtual_constructors(files, &nodes, base_len);

        // ── overlay Calls and lexical closure References ──────────────────
        // Inner scope: the name maps borrow `nodes`' strings and must drop
        // before `nodes` moves into Self.
        let mut edges: Vec<ViewEdge> = Vec::new();
        let mut out_adj: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        let mut in_adj: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        {
            let mut callables = OverlayNames::new(ResolveTarget::Callable);
            let mut types = OverlayNames::new(ResolveTarget::Type);
            let mut qualifiers = OverlayNames::new(ResolveTarget::Qualifier);
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

            // Full indexing tombstones a later same-uid declaration (a
            // half-finished edit) or keeps it as a later overload, and never
            // registers either in the global name table, so only the first
            // one of each uid is a cross-file candidate. The same-file tier still sees both: the
            // index's own-file lookup treats them as ambiguous and binds none.
            let mut named_uids: FxHashSet<u64> = FxHashSet::default();
            let mut virt_off = 0usize;
            for (file_ord, file) in files.iter().enumerate() {
                for _ in &file.symbols {
                    let node = &nodes[virt_off];
                    let virt = base_len + virt_off as u32;
                    let first_of_uid = named_uids.insert(node.uid);
                    callables.add(file_ord, node, virt, file_metas[file_ord], first_of_uid);
                    types.add(file_ord, node, virt, file_metas[file_ord], first_of_uid);
                    qualifiers.add(file_ord, node, virt, file_metas[file_ord], first_of_uid);
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

            let facts = OverlayFacts::new(graph, files, &nodes, &dirty_base);
            // The index's `emit_overload_references`, in the same order.
            let mut firsts: Vec<u32> = later_overloads.keys().copied().collect();
            firsts.sort_unstable();
            for first in firsts {
                for &target in &later_overloads[&first] {
                    push_edge(ViewEdge {
                        source: first,
                        target,
                        rel_type: RelType::References,
                        confidence: 1.0,
                        reason: crate::analyzer::types::OVERLOAD_SET_REASON,
                    });
                }
            }
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
                let first = (virt_cursor - base_len) as usize;
                emit_closure_references(
                    file,
                    &nodes[first..first + file.symbols.len()],
                    virt_cursor,
                    &mut push_edge,
                );
                let chain = rust_modules
                    .as_ref()
                    .and_then(|modules| modules.caller_chain(&file.rel_path));
                // One cache per dirty file: a repeated qualified call reuses
                // both successful and unresolved lookups without rebuilding paths.
                let mut module_calls = FxHashMap::default();
                for sym in &file.symbols {
                    let source = virt_cursor;
                    virt_cursor += 1;
                    let call = Some(CallScope {
                        owner: sym.owner_class.as_deref().and_then(owner_key),
                    });
                    for raw_callee in &sym.calls {
                        let caller = file_metas[file_ord];
                        let site = CallSite::parse(raw_callee);
                        let hit =
                            if caller.language == Language::Rust && site.name().contains("::") {
                                *module_calls.entry(site.name()).or_insert_with(|| {
                                    rust_modules
                                        .as_ref()
                                        .zip(chain.as_deref())
                                        .and_then(|(modules, chain)| {
                                            resolve_rust_module_callee(
                                                graph,
                                                site.name(),
                                                file,
                                                &callables,
                                                &nodes,
                                                &dirty_base,
                                                modules,
                                                chain,
                                            )
                                        })
                                        .or_else(|| {
                                            // A type path (`K::st`) has no module
                                            // file: the index's qualifier tier
                                            // binds it.
                                            let (qualifier, member) = split_qualifier(site.name())?;
                                            facts
                                                .qualified_callee(
                                                    file_ord,
                                                    &file.imports,
                                                    caller,
                                                    (site.name(), qualifier, member),
                                                    callables.target,
                                                    &qualifiers,
                                                )
                                                .filter(|&(idx, _)| {
                                                    rust_path_binds(graph, &nodes, idx, qualifier)
                                                })
                                        })
                                })
                            } else {
                                resolve_callee(
                                    graph,
                                    site,
                                    file_ord,
                                    &file.imports,
                                    caller,
                                    &mut base_metas,
                                    &callables,
                                    &types,
                                    &qualifiers,
                                    &nodes,
                                    &replaced,
                                    &dirty_base,
                                    &facts,
                                    call,
                                )
                            }
                            .or_else(|| {
                                let (ty, confidence) = resolve_constructed_type(
                                    graph,
                                    site,
                                    file_ord,
                                    &file.imports,
                                    caller,
                                    &mut base_metas,
                                    &types,
                                    &qualifiers,
                                    &nodes,
                                    &replaced,
                                    &dirty_base,
                                    &facts,
                                    call,
                                )?;
                                let target = *construction_targets.entry(ty).or_insert_with(|| {
                                    construction_target(graph, &nodes, &type_ctors, ty)
                                });
                                Some((target, confidence))
                            });
                        // The index drops self-recursion Calls (builder's
                        // `target_id == current_node_idx` skip).
                        if let Some((target, confidence)) = hit.filter(|&(t, _)| t != source) {
                            // The index's Pass 2 names the reason by the same
                            // confidence (`CallReasons::of`).
                            let reason = if confidence == NEAREST_COPY_CONFIDENCE {
                                NEAREST_COPY_REASON
                            } else {
                                super::merged::OVERLAY_EDGE_REASON
                            };
                            push_edge(ViewEdge {
                                source,
                                target,
                                rel_type: RelType::Calls,
                                confidence,
                                reason,
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

    /// Only lexical closure and overload-set references are rebuilt, not
    /// References generally.
    pub(crate) fn rebuilds_closure_references(&self, source: u32) -> bool {
        self.dirty_base.contains(&source)
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

/// Whether a fresh parse (a fragment's `RawNode`s) can carry a node of
/// `kind`. The graph builder makes the rest from whole-repo passes (File,
/// routes, entry points, processes, detector and path-literal nodes), so a
/// fragment never re-emits them: absence there says nothing about deletion.
/// Exhaustive on purpose — a new kind must decide which side it is on.
fn fragment_emits(kind: NodeKind) -> bool {
    match kind {
        NodeKind::Function
        | NodeKind::Class
        | NodeKind::Method
        | NodeKind::Interface
        | NodeKind::Constructor
        | NodeKind::Property
        | NodeKind::Variable
        | NodeKind::Const
        | NodeKind::Section
        | NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Typedef
        | NodeKind::Namespace
        | NodeKind::Module
        | NodeKind::Macro
        | NodeKind::Annotation
        | NodeKind::Trait
        | NodeKind::Impl
        | NodeKind::EnumVariant => true,
        // Import and Document have no producer today; keeping a node beats
        // suppressing one nothing can re-emit.
        NodeKind::File
        | NodeKind::Import
        | NodeKind::Route
        | NodeKind::Process
        | NodeKind::Document
        | NodeKind::EntryPoint
        | NodeKind::SchemaField
        | NodeKind::EventTopic
        | NodeKind::TransactionScope
        | NodeKind::PathLiteral => false,
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
    let base;
    let ctors: &[u32] = if ty >= base_len {
        type_ctors.get(&ty).map(Vec::as_slice).unwrap_or(&[])
    } else {
        base = base_constructors(graph, ty);
        &base
    };
    sole_constructor(ctors, |idx| is_later_overload(graph, nodes, idx)).unwrap_or(ty)
}

/// The one constructor among `ctors`, or `None`. Later overloads do not
/// count: the index registers only the first of them, which then stands for
/// the type's constructor.
fn sole_constructor(ctors: &[u32], is_later: impl Fn(u32) -> bool) -> Option<u32> {
    let mut firsts = ctors.iter().copied().filter(|&c| !is_later(c));
    let first = firsts.next()?;
    firsts.next().is_none().then_some(first)
}

/// [`ViewNode::later_overload`] in merged space.
fn is_later_overload(graph: &ArchivedZeroCopyGraph, nodes: &[ViewNode], idx: u32) -> bool {
    match idx.checked_sub(graph.nodes.len() as u32) {
        Some(virt_off) => nodes[virt_off as usize].later_overload,
        None => graph.is_later_overload(idx),
    }
}

/// Mirror full indexing's lexical references and first-UID survivor rule.
fn emit_closure_references(
    file: &OverlayFileInput,
    nodes: &[ViewNode],
    start: u32,
    push: &mut impl FnMut(ViewEdge),
) {
    let anonymous =
        |s: &OverlaySymbol| s.kind == NodeKind::Function && s.name.starts_with("<anonymous:");
    if !file.symbols.iter().any(anonymous) {
        return;
    }
    let functions: Vec<_> = file
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            matches!(
                s.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            )
        })
        .collect();
    let spans: Vec<_> = functions
        .iter()
        .map(|(_, s)| (s.start_line, s.start_column, s.end_line, s.end_column))
        .collect();
    let parents = innermost_enclosing(&spans);
    // Overloads carry their own uids, as in full indexing. Full indexing
    // tombstones other later identical UIDs, skips closure tombstones, and
    // redirects parent tombstones.
    let mut live_by_uid = FxHashMap::default();
    for (offset, node) in nodes.iter().enumerate() {
        live_by_uid.entry(node.uid).or_insert(offset);
    }
    for (i, &(offset, symbol)) in functions.iter().enumerate() {
        if !anonymous(symbol) || live_by_uid[&nodes[offset].uid] != offset {
            continue;
        }
        let Some(parent) = parents[i] else {
            continue;
        };
        let source = live_by_uid[&nodes[functions[parent].0].uid];
        push(ViewEdge {
            source: start + source as u32,
            target: start + offset as u32,
            rel_type: RelType::References,
            confidence: 1.0,
            reason: CLOSURE_REFERENCE_REASON,
        });
    }
}

/// Overlay symbols of one kind family (`kind`), by name: the Tier-1
/// same-file and Tier-3 overlay-wide candidates of [`resolve_callee`].
struct OverlayNames<'a> {
    /// The kind family, as the index's lookups name it.
    target: ResolveTarget,
    /// (file ordinal, name) → virtual idxs.
    same_file: FxHashMap<(usize, &'a str), Vec<u32>>,
    /// name → (virtual idx, file meta), anywhere in the overlay.
    anywhere: FxHashMap<&'a str, Vec<(u32, FileMeta)>>,
}

impl<'a> OverlayNames<'a> {
    fn new(target: ResolveTarget) -> Self {
        Self {
            target,
            same_file: FxHashMap::default(),
            anywhere: FxHashMap::default(),
        }
    }

    fn add(
        &mut self,
        file_ord: usize,
        node: &'a ViewNode,
        virt: u32,
        meta: FileMeta,
        first_of_uid: bool,
    ) {
        // The index registers a later overload by id only: no name tier,
        // same-file included, can bind it.
        if node.later_overload || !self.target.kind_predicate()(node.kind) {
            return;
        }
        self.same_file
            .entry((file_ord, node.name.as_str()))
            .or_default()
            .push(virt);
        if first_of_uid {
            self.anywhere
                .entry(node.name.as_str())
                .or_default()
                .push((virt, meta));
        }
    }
}

type StarModuleCache = FxHashMap<Box<str>, Rc<[Box<str>]>>;
type NamedImportCache = FxHashMap<(usize, ResolveTarget), FxHashMap<Box<str>, Option<u32>>>;

/// The facts the shared import rules ([`reexport`]) read over the overlay:
/// the base graph, with each dirty file's fresh parse in place of its
/// archived facts, empty imports and exports included. Lookups touch only
/// the files a rule names: the name index, a binary search of the file
/// paths, and the file's CSR rows.
struct OverlayFacts<'a> {
    graph: &'a ArchivedZeroCopyGraph,
    scope: ImportScope<'a>,
    nodes: &'a [ViewNode],
    dirty_base: &'a FxHashSet<u32>,
    /// Dirty rel_path → file ordinal.
    dirty: FxHashMap<&'a str, usize>,
    /// (file ordinal, name) → the virtual idxs of the first declaration of
    /// each uid, in source order: the index registers no later twin.
    declared: FxHashMap<(usize, &'a str), Vec<u32>>,
    /// Per virtual offset: the fresh parse's `is_exported`.
    exported: Vec<bool>,
    /// Per dirty file: whether it is an ES module.
    es_modules: Vec<bool>,
    /// Per dirty file: its [`ExportAliasTable`].
    dirty_aliases: Vec<Option<ExportAliasTable>>,
    /// Clean file index → its archived [`ExportAliasTable`], built on first
    /// read: a barrel is read once per name probed through it.
    clean_aliases: RefCell<FxHashMap<u32, Option<Rc<ExportAliasTable>>>>,
    /// Module → its resolved `export *` modules, as the index resolves them
    /// once per module.
    star_modules: RefCell<StarModuleCache>,
    /// (dirty file ordinal, kind family) → callee → the named-import tier's
    /// answer: a file calls one imported name at many sites, and each probe
    /// walks the specifier's candidate files.
    named_imports: RefCell<NamedImportCache>,
    /// File stem → files, the index's `SymbolTable::files_by_stem`. Built on
    /// the first module-file probe of the qualifier tier: one pass over
    /// `graph.files`, which most builds never pay.
    stems: OnceCell<FxHashMap<&'a str, Vec<&'a str>>>,
}

impl<'a> OverlayFacts<'a> {
    fn new(
        graph: &'a ArchivedZeroCopyGraph,
        files: &'a [OverlayFileInput],
        nodes: &'a [ViewNode],
        dirty_base: &'a FxHashSet<u32>,
    ) -> Self {
        let base_len = graph.nodes.len() as u32;
        let mut declared: FxHashMap<(usize, &'a str), Vec<u32>> = FxHashMap::default();
        let mut exported = Vec::with_capacity(nodes.len());
        let mut es_modules = Vec::with_capacity(files.len());
        let mut dirty_aliases = Vec::with_capacity(files.len());
        let mut seen_uids: FxHashSet<u64> = FxHashSet::default();
        let mut offset = 0usize;
        for (file_ord, file) in files.iter().enumerate() {
            let mut exports_any = false;
            for symbol in &file.symbols {
                let node = &nodes[offset];
                if !node.later_overload && seen_uids.insert(node.uid) {
                    declared
                        .entry((file_ord, node.name.as_str()))
                        .or_default()
                        .push(base_len + offset as u32);
                    exports_any |= symbol.is_exported;
                }
                exported.push(symbol.is_exported);
                offset += 1;
            }
            let aliases = ExportAliasTable::build(alias_pairs(file));
            es_modules.push(reexport::is_es_module(
                Language::from_normalized_path(&file.rel_path),
                &file.rel_path,
                exports_any,
                aliases.is_some(),
            ));
            dirty_aliases.push(aliases);
        }
        Self {
            graph,
            scope: ImportScope::new(graph, files, dirty_base),
            nodes,
            dirty_base,
            dirty: files
                .iter()
                .enumerate()
                .map(|(file_ord, file)| (file.rel_path.as_str(), file_ord))
                .collect(),
            declared,
            exported,
            es_modules,
            dirty_aliases,
            clean_aliases: RefCell::default(),
            star_modules: RefCell::default(),
            named_imports: RefCell::default(),
            stems: OnceCell::new(),
        }
    }

    fn base_len(&self) -> u32 {
        self.graph.nodes.len() as u32
    }

    /// [`reexport::named_import`] for `callee` of the dirty file `file_ord`,
    /// memoized per file and kind family.
    fn named_import(
        &self,
        file_ord: usize,
        imports: &[RawImport],
        callee: &str,
        target: ResolveTarget,
    ) -> Option<u32> {
        if let Some(&hit) = self
            .named_imports
            .borrow()
            .get(&(file_ord, target))
            .and_then(|hits| hits.get(callee))
        {
            return hit;
        }
        let importer = self.scope.dirty()[file_ord].rel_path.as_str();
        // The index's `may_reach` gate only skips a name that no declaration
        // and no alias carries, which no re-export can bind: a pre-filter,
        // not a rule, so following every miss here gives the same answer.
        let hit = reexport::named_import(
            self,
            importer,
            imports.iter().map(ImportRef::from),
            callee,
            target,
            |_| true,
        )
        .map(|hit| hit.decl);
        self.named_imports
            .borrow_mut()
            .entry((file_ord, target))
            .or_default()
            .insert(callee.into(), hit);
        hit
    }

    /// The stored spelling of `file`, a dirty or an indexed path.
    fn stored_path(&self, file: &str) -> Option<&'a str> {
        if let Some((&path, _)) = self.dirty.get_key_value(file) {
            return Some(path);
        }
        let graph = self.graph;
        let file_idx = graph.file_idx_of(file)?;
        Some(
            graph.files[file_idx as usize]
                .path
                .resolve(&graph.string_pool),
        )
    }

    /// The file `idx` (base or virtual) is declared in.
    fn node_file(&self, idx: u32) -> Option<&'a str> {
        let (graph, nodes) = (self.graph, self.nodes);
        match idx.checked_sub(self.base_len()) {
            Some(offset) => Some(&*nodes[offset as usize].rel_path),
            None => graph
                .files
                .get(graph.nodes[idx as usize].file_idx.to_native() as usize)
                .map(|f| f.path.resolve(&graph.string_pool)),
        }
    }

    /// The index's `SymbolTable::lookup_in_file_with_kind`, or with `owner`
    /// its `owned_in_file`: the first declaration of `name` in `file` of
    /// `target`'s kinds, in source order, whose owning type's [`owner_key`]
    /// is `owner` when one is given. A dirty file answers from its fresh
    /// parse only.
    fn first_in_file(
        &self,
        file: &str,
        name: &str,
        target: ResolveTarget,
        owner: Option<&str>,
    ) -> Option<u32> {
        let wanted = target.kind_predicate();
        let owned = |idx: u32| owner.is_none_or(|owner| self.owner_of(idx) == Some(owner));
        if let Some(&file_ord) = self.dirty.get(file) {
            let base_len = self.base_len();
            return self
                .declared
                .get(&(file_ord, name))?
                .iter()
                .copied()
                .find(|&virt| wanted(self.nodes[(virt - base_len) as usize].kind) && owned(virt));
        }
        let graph = self.graph;
        let file_idx = graph.file_idx_of(file)?;
        graph
            .nodes_by_name(name)
            .filter(|&idx| {
                let node = &graph.nodes[idx as usize];
                node.file_idx.to_native() == file_idx
                    && !graph.is_later_overload(idx)
                    && wanted(NodeKind::from(&node.kind))
                    && owned(idx)
            })
            .min()
    }

    /// The [`owner_key`] of the type declaring `idx`, `None` at top level.
    fn owner_of(&self, idx: u32) -> Option<&'a str> {
        let (graph, nodes) = (self.graph, self.nodes);
        match idx.checked_sub(self.base_len()) {
            Some(offset) => nodes[offset as usize]
                .owner_class
                .as_deref()
                .and_then(owner_key),
            None => owner_key(
                graph.nodes[idx as usize]
                    .owner_class
                    .resolve(&graph.string_pool),
            ),
        }
    }

    fn kind_of(&self, idx: u32) -> NodeKind {
        match idx.checked_sub(self.base_len()) {
            Some(offset) => self.nodes[offset as usize].kind,
            None => NodeKind::from(&self.graph.nodes[idx as usize].kind),
        }
    }

    /// The index's `SymbolTable::is_copy`: `b` repeats declaration `a` in
    /// another tree, with the same kind and owner, in a file of the same name
    /// in a directory of the same name, declaring the same namespaces.
    fn is_copy(&self, a: u32, b: u32) -> bool {
        let (Some(a_file), Some(b_file)) = (self.node_file(a), self.node_file(b)) else {
            return false;
        };
        a_file != b_file
            && self.kind_of(a) == self.kind_of(b)
            && self.owner_of(a) == self.owner_of(b)
            && leaf_path(a_file) == leaf_path(b_file)
            && self
                .scope
                .same_namespaces(Language::from_normalized_path(a_file), a_file, b_file)
    }

    /// The index's `SymbolTable::nearest_global_copy` over the Tier-3
    /// `candidates` that `caller` admits: when all of them are copies of the
    /// first ([`Self::is_copy`]), the copy whose file shares the deepest
    /// directory with `caller_file`, fewer directory steps breaking an equal
    /// depth. `None` for a tie, for fewer than two copies, or at the first
    /// candidate that is no copy, so a common name costs two comparisons.
    fn nearest_copy(
        &self,
        caller: FileMeta,
        caller_file: &str,
        candidates: impl Iterator<Item = (u32, FileMeta)>,
    ) -> Option<u32> {
        let mut copies: Vec<u32> = Vec::new();
        for (idx, meta) in candidates {
            if !caller.admits(meta) {
                continue;
            }
            if copies
                .first()
                .is_some_and(|&first| !self.is_copy(first, idx))
            {
                return None;
            }
            copies.push(idx);
        }
        if copies.len() < 2 {
            return None;
        }
        let caller_dirs = || path_dirs(caller_file);
        let mut best: Option<((usize, std::cmp::Reverse<usize>), u32)> = None;
        let mut tie = false;
        for idx in copies {
            let file = self.node_file(idx)?;
            let shared = caller_dirs()
                .zip(path_dirs(file))
                .take_while(|(a, b)| a == b)
                .count();
            let steps = caller_dirs().count() + path_dirs(file).count() - 2 * shared;
            let key = (shared, std::cmp::Reverse(steps));
            match best {
                Some((best_key, _)) if key < best_key => {}
                Some((best_key, _)) if key == best_key => tie = true,
                _ => {
                    best = Some((key, idx));
                    tie = false;
                }
            }
        }
        best.filter(|_| !tie).map(|(_, idx)| idx)
    }

    /// The index's `SymbolTable::files_by_stem`. The index leaves out a file
    /// that declares nothing, which the base graph does not record per file:
    /// such a file here can make a stem ambiguous, never add a target.
    fn files_by_stem(&self, stem: &str) -> &[&'a str] {
        let stems = self.stems.get_or_init(|| {
            let graph = self.graph;
            let base = graph
                .files
                .iter()
                .map(|f| f.path.resolve(&graph.string_pool))
                .filter(|path| !self.dirty.contains_key(path));
            let fresh = self
                .scope
                .dirty()
                .iter()
                .filter(|file| !file.symbols.is_empty())
                .map(|file| file.rel_path.as_str());
            let mut stems: FxHashMap<&'a str, Vec<&'a str>> = FxHashMap::default();
            for path in base.chain(fresh) {
                if let Some(stem) = std::path::Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                {
                    stems.entry(stem).or_default().push(path);
                }
            }
            stems
        });
        stems.get(stem).map_or(&[], Vec::as_slice)
    }

    /// The index's Tier 2.5 (`Resolver::resolve_symbol_with_import_binding`):
    /// the qualified callee `callee` = `qualifier` + `member` of the dirty
    /// file `file_ord` binds the first `member` of `target`'s kinds in the
    /// file [`Self::qualifier_file`] picks. A Python `super().m()` takes the
    /// index's base-class rule, which the overlay does not replay.
    fn qualified_callee(
        &self,
        file_ord: usize,
        imports: &[RawImport],
        caller: FileMeta,
        (callee, qualifier, member): (&str, &str, &str),
        target: ResolveTarget,
        qualifiers: &OverlayNames<'_>,
    ) -> Option<(u32, f32)> {
        if qualifier == CallSite::SUPER_RECEIVER && caller.language == Language::Python {
            return None;
        }
        let file = self.qualifier_file(
            file_ord,
            imports,
            caller,
            (callee, qualifier, member),
            target,
            qualifiers,
        )?;
        let pick = self.first_in_file(file, member, target, None)?;
        // The index's `SymbolTable::prefer_owned`: a call through `Q` reaches
        // `Q`'s own member before a same-named one of another type that
        // precedes it in the file (a trait method, a base declaration).
        let pick = match (self.owner_of(pick), owner_key(qualifier)) {
            (Some(picked), Some(wanted)) if picked != wanted => self
                .first_in_file(file, member, target, Some(wanted))
                .unwrap_or(pick),
            _ => pick,
        };
        Some((pick, CONF_QUALIFIER_SCOPED))
    }

    /// The index's `Resolver::resolve_qualifier_file`: the file that defines
    /// `qualifier` and declares `member`, by the same four tiers: the
    /// caller's own file, an import binding the qualifier, a unique
    /// project-wide definition, then a file named after the qualifier.
    ///
    /// `None` also where the overlay cannot replay the index's answer, so no
    /// edge stands in for a different one: a Rust `use` of the qualifier or a
    /// Rust path through an external module (the index walks the module
    /// tree), and a TS / JS import that is not relative (`graph.bin` keeps
    /// no tsconfig path aliases).
    fn qualifier_file(
        &self,
        file_ord: usize,
        imports: &[RawImport],
        caller: FileMeta,
        (callee, qualifier, member): (&str, &str, &str),
        target: ResolveTarget,
        qualifiers: &OverlayNames<'_>,
    ) -> Option<&'a str> {
        let source = self.scope.dirty()[file_ord].rel_path.as_str();
        let language = caller.language;
        let rust = language == Language::Rust;
        if rust
            && rust_type_path_prefix(callee, qualifier).is_some()
            && !qualifier_prefix_is_internal(callee, qualifier)
        {
            return None;
        }
        let declares_member = |file: &str| self.first_in_file(file, member, target, None).is_some();
        if qualifiers.same_file.contains_key(&(file_ord, qualifier)) && declares_member(source) {
            return Some(source);
        }

        let ecmascript = is_ecmascript(language);
        for import in imports
            .iter()
            .filter(|i| i.alias.as_deref().unwrap_or(&i.imported_name) == qualifier)
        {
            if rust || (ecmascript && !is_relative_specifier(&import.source)) {
                return None;
            }
            let exported = import.imported_name.as_str();
            if exported == "*" && import_member_fallback(language) && !fqn_language(language) {
                match self.scope.module(source, &import.source, language) {
                    Module::Unique(file) => return Some(file),
                    _ => continue,
                }
            }
            let mut hit = None;
            // `import * as ns from './a'`: the module is the scope, and a
            // miss ends the search.
            if exported == "*" && ecmascript {
                self.for_each_candidate(source, &import.source, &mut |candidate| {
                    let found =
                        reexport::lookup_export_in_file(self, candidate, member, target).is_some();
                    if found {
                        hit = self.stored_path(candidate);
                    }
                    !found
                });
                return hit;
            }
            let default_import = ImportRef::from(import).is_es_default();
            self.for_each_candidate(source, &import.source, &mut |candidate| {
                let found = reexport::imported_in_file(
                    self,
                    candidate,
                    exported,
                    default_import,
                    ResolveTarget::Qualifier,
                )
                .is_some()
                    && declares_member(candidate);
                if found {
                    hit = self.stored_path(candidate);
                }
                !found
            });
            if hit.is_some() {
                return hit;
            }
        }

        let graph = self.graph;
        let is_qualifier = ResolveTarget::Qualifier.kind_predicate();
        let base = graph
            .nodes_by_name(qualifier)
            .filter(|&idx| {
                is_qualifier(NodeKind::from(&graph.nodes[idx as usize].kind))
                    && !self.dirty_base.contains(&idx)
                    && !graph.is_later_overload(idx)
            })
            .map(|idx| {
                let meta = self
                    .node_file(idx)
                    .map_or_else(FileMeta::default, FileMeta::from_path);
                (idx, meta)
            });
        let overlay = qualifiers
            .anywhere
            .get(qualifier)
            .into_iter()
            .flatten()
            .copied();
        if let GlobalPick::Unique(id) = pick_global(caller, base.chain(overlay)) {
            if let Some(file) = self.node_file(id).filter(|&file| declares_member(file)) {
                return Some(file);
            }
        }

        // A module named by its file (`mod util;` ↔ `util.rs`), unique within
        // the caller's crate root; an external path (`std::fs::read`) never.
        if !qualifier_prefix_is_internal(callee, qualifier) {
            return None;
        }
        let root = crate_root_prefix(source);
        let named_file = |file: &str| {
            std::path::Path::new(file)
                .file_stem()
                .and_then(|s| s.to_str())
                == Some(qualifier)
                && crate_root_prefix(file) == root
        };
        // Most qualifiers (`list.add`) name no file that declares the member:
        // settle that from the member's declarations before the one pass
        // over every file path that uniqueness needs.
        let wanted = target.kind_predicate();
        let declared_in_named_file = self
            .scope
            .dirty()
            .iter()
            .map(|file| file.rel_path.as_str())
            .any(|file| named_file(file) && declares_member(file))
            || graph.nodes_by_name(member).any(|idx| {
                wanted(NodeKind::from(&graph.nodes[idx as usize].kind))
                    && !self.dirty_base.contains(&idx)
                    && self.node_file(idx).is_some_and(&named_file)
            });
        if !declared_in_named_file {
            return None;
        }
        let mut stems = self
            .files_by_stem(qualifier)
            .iter()
            .filter(|&&file| named_file(file));
        match (stems.next(), stems.next()) {
            (Some(&file), None) => Some(file),
            _ => None,
        }
    }
}

fn alias_pairs<'f>(
    file: &'f OverlayFileInput,
) -> impl Iterator<Item = (&'f str, &'f str)> + Clone + 'f {
    file.export_aliases
        .iter()
        .map(|alias| (&*alias.exported, &*alias.local))
}

impl<'a> ModuleFacts for OverlayFacts<'a> {
    type Path = &'a str;

    fn has_file(&self, file: &str) -> bool {
        self.dirty.contains_key(file) || self.graph.file_idx_of(file).is_some()
    }

    fn has_module_name(&self, language: Language, name: &str) -> bool {
        self.scope.has_module_name(language, name)
    }

    fn declares(&self, file: &str, name: &str) -> bool {
        !self.decls_in_file(file, name).is_empty()
    }

    fn decls_in_file(&self, file: &str, name: &str) -> Vec<(u32, NodeKind)> {
        if let Some(&file_ord) = self.dirty.get(file) {
            let base_len = self.base_len();
            return self
                .declared
                .get(&(file_ord, name))
                .map_or_else(Vec::new, |virts| {
                    virts
                        .iter()
                        .map(|&virt| (virt, self.nodes[(virt - base_len) as usize].kind))
                        .collect()
                });
        }
        let Some(file_idx) = self.graph.file_idx_of(file) else {
            return Vec::new();
        };
        // Only parser-made kinds: the index's file lookups never see the
        // builder's File, Route or EntryPoint nodes, nor a later overload.
        let mut decls: Vec<(u32, NodeKind)> = self
            .graph
            .nodes_by_name(name)
            .filter(|&idx| {
                self.graph.nodes[idx as usize].file_idx.to_native() == file_idx
                    && !self.graph.is_later_overload(idx)
            })
            .map(|idx| (idx, NodeKind::from(&self.graph.nodes[idx as usize].kind)))
            .filter(|&(_, kind)| fragment_emits(kind))
            .collect();
        // The name index is hash-sorted; the index's file lookups keep source
        // order, which node order is within one file.
        decls.sort_unstable_by_key(|&(idx, _)| idx);
        decls
    }

    fn is_exported(&self, decl: u32) -> bool {
        match decl.checked_sub(self.base_len()) {
            Some(offset) => self.exported[offset as usize],
            None => self.graph.is_exported_node(decl),
        }
    }

    fn is_es_module(&self, file: &str) -> bool {
        match self.dirty.get(file) {
            Some(&file_ord) => self.es_modules[file_ord],
            None => self
                .graph
                .file_idx_of(file)
                .is_some_and(|file_idx| self.graph.is_es_module_file(file_idx)),
        }
    }

    fn export_aliases(&self, file: &str) -> Option<Fact<'_, ExportAliasTable>> {
        if !export_filter_applies(file) {
            return None;
        }
        if let Some(&file_ord) = self.dirty.get(file) {
            return self.dirty_aliases[file_ord].as_ref().map(Fact::Borrowed);
        }
        let file_idx = self.graph.file_idx_of(file)?;
        if let Some(table) = self.clean_aliases.borrow().get(&file_idx) {
            return table.clone().map(Fact::Shared);
        }
        let pool = self.graph.string_pool.as_slice();
        let table = self
            .graph
            .file_export_aliases(file_idx)
            .and_then(|rows| {
                ExportAliasTable::build(
                    rows.iter()
                        .map(|row| (row.exported.resolve(pool), row.local.resolve(pool))),
                )
            })
            .map(Rc::new);
        self.clean_aliases
            .borrow_mut()
            .insert(file_idx, table.clone());
        table.map(Fact::Shared)
    }

    fn imports(&self, file: &str) -> Option<FileImports<'_>> {
        if let Some(&file_ord) = self.dirty.get(file) {
            return Some(FileImports::Parsed(&self.scope.dirty()[file_ord].imports));
        }
        let rows = self.graph.file_imports(self.graph.file_idx_of(file)?)?;
        Some(FileImports::Archived {
            rows,
            pool: self.graph.string_pool.as_slice(),
            files: self.graph.files.as_slice(),
        })
    }

    fn star_modules(&self, module: &str) -> Fact<'_, [Box<str>]> {
        if let Some(stars) = self.star_modules.borrow().get(module) {
            return Fact::Shared(Rc::clone(stars));
        }
        let stars: Rc<[Box<str>]> = match self.imports(module) {
            Some(imports) => reexport::resolve_star_modules(self, module, imports.iter()).into(),
            None => Rc::from(Vec::new()),
        };
        self.star_modules
            .borrow_mut()
            .insert(module.into(), Rc::clone(&stars));
        Fact::Shared(stars)
    }

    fn python_module(&self, importer: &str, specifier: &str) -> PyModule<&'a str> {
        match self.scope.module(importer, specifier, Language::Python) {
            Module::Unique(file) => PyModule::Unique(file),
            Module::Ambiguous => PyModule::Ambiguous,
            Module::Missing | Module::Namespace(_) => PyModule::Missing,
        }
    }

    fn for_each_candidate(
        &self,
        importer: &str,
        specifier: &str,
        visit: &mut dyn FnMut(&str) -> bool,
    ) {
        // `graph.bin` keeps no tsconfig path aliases: an aliased specifier
        // probes only its literal candidates here.
        reexport::for_each_specifier_candidate(
            std::path::Path::new(importer),
            specifier,
            |_| {},
            visit,
        );
    }
}

/// Mirror of index-time Pass-2 `Calls` resolution, narrowed to the inputs
/// available at query time, over the kind family of `names`. Returns the
/// merged-space target index.
/// Rust `::` calls use the cached module-path branch in `build` first.
/// Method eligibility is a post-filter, like index-time `lookup_call_in_file`:
/// removing free functions before uniqueness checks would invent a winner.
///
/// A Python call on a from-imported submodule binds first, as in the index
/// ([`reexport::python_submodule_member`]).
/// Tier 1 — same file: the dirty file was FULLY re-parsed, so its own
/// callable set is authoritative. Unique match → confidence 1.0. Among
/// several, a call from a member of `K` takes `K`'s own one when the first
/// belongs to a type, as the index's `SymbolTable::prefer_owned` does.
/// Tier 2 — import-scoped. In an [`import_member_fallback`] language,
/// [`bind_import`] replays the index's import-member tier, a Python package
/// re-export included. A TS / JS caller takes the index's named-import tier
/// itself ([`reexport::named_import`]: export eligibility, export aliases,
/// default imports and the one-hop barrel re-export); a non-relative
/// specifier it misses keeps the segment match below. Elsewhere
/// [`import_binding`] maps the callee to the name the import declares, as the
/// index's named-import tier does. A relative source (`./`, `../`) probes the
/// index's candidate files in its order ([`relative_module_rank`]) and the
/// first one declaring that name wins; any other source narrows the
/// candidates to files matching its last path segment, unique only. Hit →
/// 0.95.
/// Tier 2.5 — qualifier-scoped: a qualified callee (`K.save`, `K::st`)
/// takes the member in the qualifier's file
/// ([`OverlayFacts::qualified_callee`]) → 0.85, or no edge; it never reaches
/// Tier 3.
/// Tier 3 — global: all clean-base callables (via the archived `name_index`)
/// plus all overlay callables, through the index-time candidate filter
/// [`pick_global`] (language and vendor barriers, unique only). ≥2
/// remaining → suppressed, matching `DecisionTier::AmbiguousGlobal` (an
/// invented edge is worse than a missing one), unless `call` is given and
/// every candidate is a copy of one declaration: then the copy nearest the
/// caller binds ([`OverlayFacts::nearest_copy`]) at
/// [`NEAREST_COPY_CONFIDENCE`]. A TS / JS import naming the ES module of a
/// private pick hides it ([`reexport::import_hides`]). Unique → 0.7.
#[allow(clippy::too_many_arguments)]
fn resolve_callee<'s>(
    graph: &ArchivedZeroCopyGraph,
    site: CallSite<'s>,
    file_ord: usize,
    imports: &'s [RawImport],
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    names: &OverlayNames<'_>,
    types: &OverlayNames<'_>,
    qualifiers: &OverlayNames<'_>,
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    call: Option<CallScope<'_>>,
) -> Option<(u32, f32)> {
    let callee = site.name();
    let accepts = |idx: u32| {
        !(site.uses_method_syntax() && caller.language == Language::Rust)
            || match idx.checked_sub(graph.nodes.len() as u32) {
                Some(offset) => nodes[offset as usize].kind == NodeKind::Method,
                None => NodeKind::from(&graph.nodes[idx as usize].kind) == NodeKind::Method,
            }
    };
    let importer = facts.scope.dirty()[file_ord].rel_path.as_str();
    if let CallSite::FromReceiver { receiver, member } = site {
        if caller.language == Language::Python {
            if let Some((target, _)) = reexport::python_submodule_member(
                facts,
                importer,
                (receiver, member),
                imports.iter().map(ImportRef::from),
                names.target,
            ) {
                return accepts(target).then_some((target, CONF_IMPORT_SCOPED));
            }
        }
    }
    // Tier 1: same-file.
    if let Some(virts) = names.same_file.get(&(file_ord, callee)) {
        let pick = match virts.as_slice() {
            &[only] => Some(only),
            // The index takes the first, then the caller's owner's own member
            // when the first belongs to another type. A first at top level,
            // or no own member, binds the first there; the overlay leaves
            // that ambiguity (same-name free functions, same-uid twins)
            // unresolved. A same-file free function keeps a Rust method call
            // unresolved, as `pick_call` does.
            &[first, ..] => call
                .and_then(|scope| scope.owner)
                .filter(|_| facts.owner_of(first).is_some() && virts.iter().all(|&v| accepts(v)))
                .and_then(|owner| {
                    virts
                        .iter()
                        .copied()
                        .find(|&v| facts.owner_of(v) == Some(owner))
                }),
            &[] => None,
        };
        return pick.filter(|&v| accepts(v)).map(|v| (v, CONF_SAME_FILE));
    }

    // The index binds no import for an untyped member call, and in these
    // languages has no path-segment import tier either.
    let member_policy = import_member_fallback(caller.language);
    let binds_imports = !imports.is_empty() && !matches!(site, CallSite::UntypedMember(_));
    if member_policy && binds_imports {
        match bind_import(
            graph,
            callee,
            file_ord,
            imports,
            caller.language,
            names,
            types,
            nodes,
            dirty_base,
            facts,
        ) {
            Some(ImportBinding::Resolved(hit)) => return hit,
            Some(ImportBinding::Fallback(name)) => {
                return resolve_callee(
                    graph,
                    CallSite::Plain(name),
                    file_ord,
                    &[],
                    caller,
                    base_metas,
                    names,
                    types,
                    qualifiers,
                    nodes,
                    replaced,
                    dirty_base,
                    facts,
                    call,
                );
            }
            // The index's `extension_fallback` resolves outside a call scope.
            Some(ImportBinding::Extension(member)) => {
                let hit = resolve_callee(
                    graph,
                    CallSite::Plain(member),
                    file_ord,
                    imports,
                    caller,
                    base_metas,
                    names,
                    types,
                    qualifiers,
                    nodes,
                    replaced,
                    dirty_base,
                    facts,
                    None,
                );
                return hit.filter(|&(idx, _)| is_top_level(graph, nodes, idx));
            }
            None => {}
        }
    }
    let ecmascript = is_ecmascript(caller.language);
    if ecmascript && binds_imports {
        if let Some(decl) = facts.named_import(file_ord, imports, callee, names.target) {
            debug_assert!(!replaced.contains_key(&decl));
            return accepts(decl).then_some((decl, CONF_IMPORT_SCOPED));
        }
    }

    // Clean-base candidates via the name index. Dirty-file base nodes are
    // excluded here: replaced ones already participate as overlay callables
    // (same name), suppressed ones no longer exist. A later overload is in
    // no name table of the index.
    let clean_base = |name: &str| -> Vec<u32> {
        graph
            .nodes_by_name(name)
            .filter(|&idx| {
                names.target.kind_predicate()(NodeKind::from(&graph.nodes[idx as usize].kind))
                    && !dirty_base.contains(&idx)
                    && !graph.is_later_overload(idx)
            })
            .collect()
    };
    let base_candidates = clean_base(callee);
    let overlay_candidates: &[(u32, FileMeta)] =
        names.anywhere.get(callee).map(Vec::as_slice).unwrap_or(&[]);

    // Tier 2: import-scoped, the index's named-import tier: every import
    // binding the callee, in order, until one's module declares the name.
    let segment_imports: &[RawImport] = if member_policy { &[] } else { imports };
    let base_len = graph.nodes.len() as u32;
    let base_path = |idx: u32| {
        graph
            .files
            .get(graph.nodes[idx as usize].file_idx.to_native() as usize)
            .map(|f| f.path.resolve(&graph.string_pool))
    };
    for (import, declared) in segment_imports
        .iter()
        // A TS / JS relative import was probed exactly above. `graph.bin`
        // keeps no tsconfig paths, so an aliased or package specifier keeps
        // the segment approximation.
        .filter(|i| !ecmascript || !is_relative_specifier(&i.source))
        .filter_map(|i| Some((i, import_binding(i, callee, caller.language)?.name)))
    {
        let base_declared;
        let (base_scoped, overlay_scoped): (&[u32], &[(u32, FileMeta)]) = if declared == callee {
            (base_candidates.as_slice(), overlay_candidates)
        } else {
            base_declared = clean_base(declared);
            (
                base_declared.as_slice(),
                names
                    .anywhere
                    .get(declared)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
        };
        // Base candidates exclude every dirty-base node, so a hit never needs
        // a replaced-base redirect and never lands on a suppressed node:
        // surviving dirty symbols compete with their virtual index instead.
        let scoped = base_scoped.iter().map(|&idx| (idx, base_path(idx))).chain(
            overlay_scoped
                .iter()
                .map(|&(virt, _)| (virt, Some(&*nodes[(virt - base_len) as usize].rel_path))),
        );
        let hit = match is_relative_specifier(&import.source) {
            // The first probed file declaring the name, then its first
            // declaration: base and virtual indices both follow source order
            // within a file, and one file is either clean or dirty.
            true => scoped
                .filter_map(|(idx, path)| {
                    Some((relative_module_rank(importer, &import.source, path?)?, idx))
                })
                .min()
                .map(|(_, idx)| idx),
            // A namespace import of a package path names a module the
            // segment match can only guess; the index probes it as written.
            false if import.imported_name == "*" => None,
            false => {
                let segment = import_last_segment(&import.source);
                let mut matching = scoped.filter(|&(_, path)| {
                    !segment.is_empty() && path.is_some_and(|p| path_matches_segment(p, segment))
                });
                match (matching.next(), matching.next()) {
                    (Some((idx, _)), None) => Some(idx),
                    _ => None,
                }
            }
        };
        if let Some(target) = hit {
            debug_assert!(!replaced.contains_key(&target));
            return accepts(target).then_some((target, CONF_IMPORT_SCOPED));
        }
    }

    // Tier 2.5: a qualified callee resolves through its qualifier or not at
    // all; the index never retries the bare member (`std::fs::read` must
    // not bind a project `read`).
    if let Some((qualifier, member)) = split_qualifier(callee) {
        let target = match names.target {
            ResolveTarget::Callable
                if caller.language == Language::Rust && site.uses_method_syntax() =>
            {
                ResolveTarget::Method
            }
            target => target,
        };
        return facts.qualified_callee(
            file_ord,
            imports,
            caller,
            (callee, qualifier, member),
            target,
            qualifiers,
        );
    }

    // Tier 3: the shared candidate filter over clean base + overlay. Same
    // invariant as Tier 2: a base candidate is clean by construction, never
    // redirected; overlay candidates are virtual indices.
    let mut base_meta = |idx: u32| {
        let file_idx = graph.nodes[idx as usize].file_idx.to_native() as usize;
        *base_metas.entry(file_idx).or_insert_with(|| {
            graph
                .files
                .get(file_idx)
                .map_or_else(FileMeta::default, |f| {
                    FileMeta::from_path(f.path.resolve(&graph.string_pool))
                })
        })
    };
    let pick = pick_global(
        caller,
        base_candidates
            .iter()
            .map(|&idx| (idx, base_meta(idx)))
            .chain(overlay_candidates.iter().copied()),
    );
    let (target, confidence) = match pick {
        GlobalPick::Unique(target) => (target, CONF_GLOBAL_UNIQUE),
        // The index's `DecisionTier::NearestCopy`, for call sites only.
        GlobalPick::Ambiguous if call.is_some() => (
            facts.nearest_copy(
                caller,
                importer,
                base_candidates
                    .iter()
                    .map(|&idx| (idx, base_meta(idx)))
                    .chain(overlay_candidates.iter().copied()),
            )?,
            NEAREST_COPY_CONFIDENCE,
        ),
        GlobalPick::Ambiguous | GlobalPick::NoMatch => return None,
    };
    debug_assert!(!replaced.contains_key(&target));
    let decl_file = match target.checked_sub(base_len) {
        Some(offset) => Some(&*nodes[offset as usize].rel_path),
        None => base_path(target),
    };
    if decl_file.is_some_and(|file| {
        reexport::import_hides(
            facts,
            importer,
            callee,
            imports.iter().map(ImportRef::from),
            file,
        )
    }) {
        return None;
    }
    accepts(target).then_some((target, confidence))
}

/// What a call site adds to a name lookup, as the index's `CallScope`
/// (`Resolver::resolve_call`). Its presence also admits the nearest-copy
/// rule of Tier 3.
#[derive(Clone, Copy)]
struct CallScope<'c> {
    /// The [`owner_key`] of the type the calling member belongs to.
    owner: Option<&'c str>,
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
        // `PathBuf::join` writes `\` on Windows; archived paths use `/`, as
        // the analyzer's path normalisation does.
        let requested = match path.to_str()? {
            raw if raw.contains('\\') => std::borrow::Cow::Owned(raw.replace('\\', "/")),
            raw => std::borrow::Cow::Borrowed(raw),
        };
        let requested = requested.as_ref();
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
            && names.target.kind_predicate()(NodeKind::from(&node.kind))
            && NodeKind::from(&node.kind) != NodeKind::Method
            && !graph.is_later_overload(idx)
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
/// edge's target. The whole type path resolves first, as the index does:
/// `u.Widget()` binds through `import pkg as u`, `Outer.Inner()` through
/// the qualifier tier. A qualified path that does not resolve retries its
/// last segment only where [`CallSite::constructed_type`] allows that
/// fallback.
#[allow(clippy::too_many_arguments)]
fn resolve_constructed_type<'s>(
    graph: &ArchivedZeroCopyGraph,
    site: CallSite<'s>,
    file_ord: usize,
    imports: &'s [RawImport],
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    types: &OverlayNames<'_>,
    qualifiers: &OverlayNames<'_>,
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    call: Option<CallScope<'_>>,
) -> Option<(u32, f32)> {
    let (type_path, last_segment_fallback) = site.constructed_type(caller.language)?;
    let type_name = type_path
        .rsplit(['.', ':', '\\'])
        .next()
        .unwrap_or(type_path);
    let qualified = type_name.len() < type_path.len();
    // Most unresolved calls name no type at all: reject them before the
    // candidate collection in `resolve_callee` allocates. An import alias
    // names no declared type, so it passes through its imported name, as
    // the index's `names_constructible_type` gate does.
    let names_constructible = |name: &str| {
        types.anywhere.contains_key(name)
            || graph
                .nodes_by_name(name)
                .any(|idx| NodeKind::from(&graph.nodes[idx as usize].kind).is_constructible())
    };
    if !names_constructible(type_name)
        && !imports.iter().any(|import| {
            import.alias.as_deref() == Some(type_name)
                && names_constructible(import.imported_name.as_str())
        })
    {
        return None;
    }
    let mut resolve = |name: &'s str, imports: &'s [RawImport]| {
        resolve_callee(
            graph,
            CallSite::Plain(name),
            file_ord,
            imports,
            caller,
            base_metas,
            types,
            types,
            qualifiers,
            nodes,
            replaced,
            dirty_base,
            facts,
            call,
        )
    };
    // A qualified type path (`new \App\User()`) never names its type
    // through a `use` of its last segment.
    let retry_imports: &'s [RawImport] = if fqn_language(caller.language) {
        &[]
    } else {
        imports
    };
    let (ty, confidence) = resolve(type_path, imports).or_else(|| {
        (qualified && last_segment_fallback).then(|| resolve(type_name, retry_imports))?
    })?;
    let kind = match ty.checked_sub(graph.nodes.len() as u32) {
        Some(virt_off) => nodes[virt_off as usize].kind,
        None => NodeKind::from(&graph.nodes[ty as usize].kind),
    };
    kind.is_constructible().then_some((ty, confidence))
}

/// How the import-member policy settles one callee.
enum ImportBinding<'s> {
    /// The import tier's target, or no edge: every binding import is external.
    Resolved(Option<(u32, f32)>),
    /// Resolve this name through the remaining tiers, without imports.
    Fallback(&'s str),
    /// A Kotlin call through a class binding retries its bare member, and
    /// only a top-level function (an extension on that type) may answer.
    Extension(&'s str),
}

/// One pass of the import-member tier over the explicit or the wildcard
/// imports (the resolver's `import_member_hit`).
enum MemberHit {
    Bound(u32),
    /// No unique member. `bound` says whether an import of the pass binds
    /// the callee at all.
    Unbound {
        bound: bool,
    },
}

/// The index's import-member tier (`Resolver::resolve_symbol_with_import_binding`)
/// for an [`import_member_fallback`] language. `None` when no import binds
/// `callee`.
///
/// An explicit import binding `callee` whose module holds the member decides
/// the target; two explicit imports that hold different members leave the
/// general tiers to decide. When none does, a binding whose module may be
/// local (relative, single segment, first segment an indexed module name, or
/// module found) keeps the general tiers, under the member's short name for
/// Python. Only when every binding import is certainly external is there no
/// edge: a wrong suppression would drop a genuine caller. A wildcard binds
/// only when no explicit import binds the name. The index ranks it below the
/// caller's heritage too; the overlay has no heritage tier, so a heritage
/// member called through a class that also star-imports the name binds the
/// wildcard here.
#[allow(clippy::too_many_arguments)]
fn bind_import<'s>(
    graph: &ArchivedZeroCopyGraph,
    callee: &'s str,
    file_ord: usize,
    imports: &'s [RawImport],
    language: Language,
    names: &OverlayNames<'_>,
    types: &OverlayNames<'_>,
    nodes: &[ViewNode],
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
) -> Option<ImportBinding<'s>> {
    let source_file = facts.scope.dirty()[file_ord].rel_path.as_str();
    let pass = |wildcard: bool| {
        import_member_hit(
            graph,
            callee,
            source_file,
            imports,
            language,
            names,
            types,
            nodes,
            dirty_base,
            facts,
            wildcard,
        )
    };
    let explicit = pass(false);
    if let MemberHit::Bound(target) = explicit {
        return Some(ImportBinding::Resolved(Some((target, CONF_IMPORT_SCOPED))));
    }
    let mut retry = None;
    let mut extension = None;
    let mut bound = false;
    let mut external = true;
    for import in imports {
        let Some(binding) = import_binding(import, callee, language) else {
            continue;
        };
        bound = true;
        retry = retry_name(import, binding.name, callee, language);
        extension = extension.or(extension_retry(&binding, language));
        external = external && facts.scope.is_external(source_file, import, language);
    }
    if !bound {
        return None;
    }
    if external {
        return Some(extension.map_or(ImportBinding::Resolved(None), ImportBinding::Extension));
    }
    if matches!(explicit, MemberHit::Unbound { bound: false }) && fqn_language(language) {
        if let MemberHit::Bound(target) = pass(true) {
            return Some(ImportBinding::Resolved(Some((target, CONF_IMPORT_SCOPED))));
        }
    }
    // The overlay has no receiver ladder; the index's last resort for a
    // class-bound Kotlin callee is the extension retry.
    Some(extension.map_or(
        ImportBinding::Fallback(retry.unwrap_or(callee)),
        ImportBinding::Extension,
    ))
}

/// The resolver's `import_member_hit`: the member the explicit (or the
/// wildcard) imports binding `callee` hold in their modules. Python keeps the
/// first hit in source order; in a fully qualified language two different
/// hits are no hit.
#[allow(clippy::too_many_arguments)]
fn import_member_hit(
    graph: &ArchivedZeroCopyGraph,
    callee: &str,
    source_file: &str,
    imports: &[RawImport],
    language: Language,
    names: &OverlayNames<'_>,
    types: &OverlayNames<'_>,
    nodes: &[ViewNode],
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    wildcard: bool,
) -> MemberHit {
    let mut found = None;
    let mut bound = false;
    for import in imports {
        let Some(binding) =
            import_binding(import, callee, language).filter(|b| b.wildcard == wildcard)
        else {
            continue;
        };
        bound = true;
        let in_file = |file: &str| {
            let owner_agrees = match binding.owner {
                MemberOwner::Type(path) => {
                    owner_path_agrees_in_file(graph, types, nodes, dirty_base, file, path)
                }
                MemberOwner::Any | MemberOwner::TopLevel => true,
            };
            if !owner_agrees {
                return None;
            }
            member_in_file(
                graph,
                names,
                nodes,
                dirty_base,
                file,
                binding.name,
                binding.owner,
            )
        };
        let hit = match facts.scope.module(source_file, &import.source, language) {
            Module::Unique(file) => in_file(file).or_else(|| {
                reexport::python_import_reexport(facts, language, file, binding.name, names.target)
            }),
            Module::Namespace(files) => {
                let mut hits = files.iter().filter_map(|&file| in_file(file));
                hits.next().filter(|_| hits.next().is_none())
            }
            Module::Missing | Module::Ambiguous => None,
        };
        let Some(target) = hit else {
            continue;
        };
        if !fqn_language(language) {
            return MemberHit::Bound(target);
        }
        match found {
            Some(previous) if previous != target => return MemberHit::Unbound { bound },
            Some(_) => {}
            None => found = Some(target),
        }
    }
    found.map_or(MemberHit::Unbound { bound }, MemberHit::Bound)
}

/// Whether `idx` (base or virtual) is declared outside any owning type.
fn is_top_level(graph: &ArchivedZeroCopyGraph, nodes: &[ViewNode], idx: u32) -> bool {
    let owner = match idx.checked_sub(graph.nodes.len() as u32) {
        Some(virt) => nodes[virt as usize].owner_class.as_deref(),
        None => Some(
            graph.nodes[idx as usize]
                .owner_class
                .resolve(&graph.string_pool),
        ),
    };
    owner.and_then(owner_key).is_none()
}

/// The index's `SymbolTable::owner_path_agrees`: [`owner_path_agrees`] over
/// the types `file` declares. A dirty file answers from its fresh parse only,
/// first declaration of each uid, as the index tombstones the rest.
fn owner_path_agrees_in_file(
    graph: &ArchivedZeroCopyGraph,
    types: &OverlayNames<'_>,
    nodes: &[ViewNode],
    dirty_base: &FxHashSet<u32>,
    file: &str,
    path: &str,
) -> bool {
    let base_len = graph.nodes.len() as u32;
    owner_path_agrees(
        path,
        |name| {
            let base = graph
                .nodes_by_name(name)
                .filter(|&idx| {
                    let node = &graph.nodes[idx as usize];
                    NodeKind::from(&node.kind).is_type()
                        && !dirty_base.contains(&idx)
                        && graph
                            .files
                            .get(node.file_idx.to_native() as usize)
                            .is_some_and(|f| f.path.resolve(&graph.string_pool) == file)
                })
                .map(|idx| {
                    owner_key(
                        graph.nodes[idx as usize]
                            .owner_class
                            .resolve(&graph.string_pool),
                    )
                });
            let fresh = types
                .anywhere
                .get(name)
                .into_iter()
                .flatten()
                .map(|&(virt, _)| &nodes[(virt - base_len) as usize])
                .filter(|node| node.rel_path.as_ref() == file)
                .map(|node| node.owner_class.as_deref().and_then(owner_key));
            let mut owners = base.chain(fresh);
            match (owners.next(), owners.next()) {
                (None, _) => DeclaredType::Undeclared,
                (Some(_), Some(_)) => DeclaredType::Ambiguous,
                (Some(owner), None) => DeclaredType::Unique(owner),
            }
        },
        owner_key,
    )
}

/// The index's `lookup_member_in_file` over the kind family of `names`: the
/// first `member` declared in `file` whose owner satisfies `owner`. A dirty
/// file answers from its fresh parse only.
fn member_in_file(
    graph: &ArchivedZeroCopyGraph,
    names: &OverlayNames<'_>,
    nodes: &[ViewNode],
    dirty_base: &FxHashSet<u32>,
    file: &str,
    member: &str,
    owner: MemberOwner<'_>,
) -> Option<u32> {
    let owned = |owner_class: Option<&str>| match owner {
        MemberOwner::Any => true,
        MemberOwner::TopLevel => owner_class.and_then(owner_key).is_none(),
        MemberOwner::Type(ty) => owner_class
            .and_then(owner_key)
            .is_some_and(|key| Some(key) == owner_key(ty)),
    };
    let base_len = graph.nodes.len() as u32;
    graph
        .nodes_by_name(member)
        .filter(|&idx| {
            let node = &graph.nodes[idx as usize];
            names.target.kind_predicate()(NodeKind::from(&node.kind))
                && !dirty_base.contains(&idx)
                && graph
                    .files
                    .get(node.file_idx.to_native() as usize)
                    .is_some_and(|f| f.path.resolve(&graph.string_pool) == file)
                && owned(Some(node.owner_class.resolve(&graph.string_pool)))
        })
        .min()
        .or_else(|| {
            names
                .anywhere
                .get(member)?
                .iter()
                .map(|&(virt, _)| (virt, &nodes[(virt - base_len) as usize]))
                .find(|(_, node)| {
                    node.rel_path.as_ref() == file && owned(node.owner_class.as_deref())
                })
                .map(|(virt, _)| virt)
        })
}

/// Whether `specifier` is a `./` or `../` import: the overlay probes the
/// index's candidate files only for these.
fn is_relative_specifier(specifier: &str) -> bool {
    specifier.starts_with("./") || specifier.starts_with("../")
}

/// Where `path` falls in the index's probe order for `specifier` of
/// `importer` ([`reexport::for_each_specifier_candidate`], no path aliases).
/// `None` when the index never probes it.
fn relative_module_rank(importer: &str, specifier: &str, path: &str) -> Option<usize> {
    let mut rank = 0;
    let mut found = None;
    reexport::for_each_specifier_candidate(
        std::path::Path::new(importer),
        specifier,
        |_| {},
        |candidate| {
            if candidate == path {
                found = Some(rank);
                return false;
            }
            rank += 1;
            true
        },
    );
    found
}

/// The resolver's `split_qualifier`: `(qualifier, member)` of a qualified
/// callee, where the qualifier is the segment right before the rightmost
/// `::`, `.` or `\` (`std::vec::Vec::new` → `("Vec", "new")`). `None` for a
/// bare name or an empty side.
fn split_qualifier(name: &str) -> Option<(&str, &str)> {
    let (split_idx, sep_len) = [
        (name.rfind("::"), 2usize),
        (name.rfind('.'), 1),
        (name.rfind('\\'), 1),
    ]
    .into_iter()
    .filter_map(|(idx, len)| Some((idx?, len)))
    .max_by_key(|&(idx, _)| idx)?;
    let (before, after) = name.split_at(split_idx);
    let member = &after[sep_len..];
    if before.is_empty() || member.is_empty() {
        return None;
    }
    let qualifier = before
        .rsplit_once("::")
        .or_else(|| before.rsplit_once('.'))
        .or_else(|| before.rsplit_once('\\'))
        .map_or(before, |(_, q)| q);
    (!qualifier.is_empty()).then_some((qualifier, member))
}

/// The resolver's `qualifier_prefix_is_internal`: the path before
/// `qualifier` in `full_callee` is empty or a `crate` / `self` / `super`
/// chain, so the qualifier may name a project module (`std::fs::read` →
/// false, `crate::util::f` → true).
fn qualifier_prefix_is_internal(full_callee: &str, qualifier: &str) -> bool {
    let Some(member_split) = full_callee.rfind("::").or_else(|| full_callee.rfind('.')) else {
        return false;
    };
    let before_member = &full_callee[..member_split];
    let preceding = before_member
        .rsplit_once("::")
        .or_else(|| before_member.rsplit_once('.'))
        .map_or("", |(p, q)| if q == qualifier { p } else { "" });
    preceding.is_empty()
        || preceding
            .split("::")
            .all(|s| matches!(s, "crate" | "self" | "super"))
}

/// The resolver's `rust_type_path_prefix`: the module path `a::b` that the
/// Rust path call `a::b::Q::m` names before its qualifier `Q`.
fn rust_type_path_prefix<'c>(full_callee: &'c str, qualifier: &str) -> Option<&'c str> {
    let (before_member, _) = full_callee.rsplit_once("::")?;
    let (prefix, q) = before_member.rsplit_once("::")?;
    (q == qualifier).then_some(prefix)
}

/// The index's `crate_root_prefix` over a `/`-normalized path: everything
/// before the last `/src/` or `/tests/`, else empty.
fn crate_root_prefix(path: &str) -> &str {
    path.rsplit_once("/src/")
        .or_else(|| path.rsplit_once("/tests/"))
        .map_or("", |(root, _)| root)
}

/// Whether the Rust path call `qualifier::m` may bind `idx`: a method only
/// when `qualifier` is its owning type (`K::st`). A module path
/// (`crate::x::parse`) never binds a method, as in
/// [`resolve_rust_module_callee`], though the index's qualifier tier would.
fn rust_path_binds(
    graph: &ArchivedZeroCopyGraph,
    nodes: &[ViewNode],
    idx: u32,
    qualifier: &str,
) -> bool {
    let (kind, owner) = match idx.checked_sub(graph.nodes.len() as u32) {
        Some(offset) => {
            let node = &nodes[offset as usize];
            (node.kind, node.owner_class.as_deref())
        }
        None => {
            let node = &graph.nodes[idx as usize];
            (
                NodeKind::from(&node.kind),
                Some(node.owner_class.resolve(&graph.string_pool)),
            )
        }
    };
    kind != NodeKind::Method || owner.and_then(owner_key) == Some(qualifier)
}

/// The index's `leaf_path`: the last two components of `path`, its
/// directory's name and its file name (`collect/Maps.java`), or the whole
/// path at the root.
fn leaf_path(path: &str) -> &str {
    let Some((dir, _)) = path.rsplit_once('/') else {
        return path;
    };
    dir.rfind('/').map_or(path, |slash| &path[slash + 1..])
}

/// The index's `path_dirs`: the directories of `path`, root first.
fn path_dirs(path: &str) -> impl Iterator<Item = &str> {
    path.rsplit_once('/')
        .into_iter()
        .flat_map(|(dir, _)| dir.split('/'))
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
            is_exported: false,
            name: name.to_string(),
            kind: NodeKind::Function,
            owner_class: None,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            calls: calls.iter().map(|c| c.to_string()).collect(),
        }
    }

    #[test]
    fn test_build_closure_uid_collisions_use_first_live_nodes() {
        let bytes = GraphFixture::new().into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let mut parent = sym("enclosing", &[]);
        parent.end_line = 4;
        let mut overload = parent.clone();
        overload.start_line = 5;
        overload.end_line = 9;
        let mut closure = sym("<anonymous:5:2>", &[]);
        closure.start_line = 6;
        closure.end_line = 7;
        let file = OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: "App.java".into(),
            symbols: vec![parent, overload, closure.clone(), closure],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let references: Vec<_> = view
            .edges()
            .iter()
            .map(|edge| (edge.source, edge.target, edge.rel_type, edge.confidence))
            .collect();
        assert_eq!(references, vec![(0, 2, RelType::References, 1.0)]);
    }

    #[test]
    fn test_build_overload_twins_redirect_base_to_closure_parent() {
        let mut fx = GraphFixture::new();
        let base_enclosing = fx.func("App.java", "enclosing");
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let mut parent = sym("enclosing", &[]);
        parent.end_line = 4;
        let mut overload = parent.clone();
        overload.start_line = 5;
        overload.end_line = 9;
        let mut closure = sym("<anonymous:6:2>", &[]);
        closure.start_line = 6;
        closure.end_line = 7;
        let file = OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: "App.java".into(),
            symbols: vec![parent, overload, closure],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let parent = view.redirect(base_enclosing).unwrap();
        let closure = view.base_len() + 2;
        assert!(
            view.overlay_out(parent)
                .any(|(_, e)| e.target == closure && e.rel_type == RelType::References),
            "a clean caller of the base overload must reach the closure downstream"
        );
    }

    fn dirty_input() -> OverlayFileInput {
        OverlayFileInput {
            export_aliases: Vec::new(),
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

    /// Module paths built with `PathBuf::join` carry `\\` on Windows; the
    /// lookup still finds the archived `/` path.
    #[test]
    fn test_rust_module_file_lookup_backslash_path_finds_archived_path() {
        let mut fixture = GraphFixture::new();
        fixture.file("src/bin/util.rs");
        let bytes = fixture.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let modules = RustModules::new(graph, &[]);
        assert_eq!(
            modules.file(std::path::Path::new("src\\bin\\util.rs")),
            Some("src/bin/util.rs")
        );
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
            export_aliases: Vec::new(),
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
            export_aliases: Vec::new(),
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
            export_aliases: Vec::new(),
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

    /// The index binds a construction to the first constructor overload; a
    /// later one is no second constructor, so the overlay must not count it.
    #[test]
    fn test_build_construction_of_overloaded_dirty_type_calls_first_constructor_like_index() {
        let bytes = construction_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let ctor = |line: u32| OverlaySymbol {
            is_exported: false,
            name: "Multi".to_string(),
            kind: NodeKind::Constructor,
            owner_class: Some("Multi".to_string()),
            start_line: line,
            end_line: line + 1,
            start_column: 0,
            end_column: 0,
            calls: vec![],
        };
        let multi = OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: "src/Multi.java".to_string(),
            symbols: vec![
                OverlaySymbol {
                    is_exported: false,
                    kind: NodeKind::Class,
                    owner_class: None,
                    end_line: 9,
                    start_column: 0,
                    end_column: 0,
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
            export_aliases: Vec::new(),
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
            export_aliases: Vec::new(),
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
            is_exported: false,
            name: name.to_string(),
            kind,
            owner_class: Some(owner.to_string()),
            start_line: line,
            end_line: line + 1,
            start_column: 0,
            end_column: 0,
            calls: vec![],
        };
        let outer = OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: "src/outer.ts".to_string(),
            symbols: vec![
                symbol("Inner", NodeKind::Class, "Outer<T>", 2),
                symbol("constructor", NodeKind::Constructor, "Outer<T>.Inner", 3),
            ],
            imports: vec![],
        };
        let new_inner = CallSite::construct("Inner");
        let app = OverlayFileInput {
            export_aliases: Vec::new(),
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

    /// Contract: later overloads count as their first node in any order;
    /// two distinct constructors are several.
    #[test]
    fn test_sole_constructor_overloads_and_distinct_returns_first_or_none() {
        let later = |idx: u32| idx == 5;
        assert_eq!(sole_constructor(&[3, 5], later), Some(3));
        assert_eq!(sole_constructor(&[5, 3], later), Some(3));
        assert_eq!(sole_constructor(&[3, 7, 5], later), None);
        assert_eq!(sole_constructor(&[7], later), Some(7));
        assert_eq!(sole_constructor(&[], later), None);
    }

    /// Contract: builder-made kinds are never in a fragment, so a dirty file
    /// keeps them as indexed; every other kind is the parser's to re-emit.
    #[test]
    fn test_fragment_emits_builder_made_kinds_false_parser_kinds_true() {
        const BUILDER_MADE: [NodeKind; 10] = [
            NodeKind::File,
            NodeKind::Route,
            NodeKind::Process,
            NodeKind::EntryPoint,
            NodeKind::SchemaField,
            NodeKind::EventTopic,
            NodeKind::TransactionScope,
            NodeKind::PathLiteral,
            NodeKind::Import,
            NodeKind::Document,
        ];
        for kind in NodeKind::ALL {
            assert_eq!(
                fragment_emits(kind),
                !BUILDER_MADE.contains(&kind),
                "{kind:?}"
            );
        }
    }

    /// The index's relative candidates: `./` and `../` from the importer's
    /// directory, nothing for any other specifier.
    #[test]
    fn test_relative_module_rank_relative_specifiers_join_importer_dir() {
        let cases = [
            ("src/b.ts", "./a", "src/a.ts"),
            ("src/b.ts", "./a.ts", "src/a.ts"),
            ("src/b.ts", "./lib/", "src/lib/index.ts"),
            ("src/x/b.ts", "../a", "src/a.ts"),
            ("src/x/b.ts", "../../a", "a.ts"),
            ("b.ts", "../a", "a.ts"),
            ("b.ts", "./a", "a.ts"),
        ];
        for (importer, specifier, path) in cases {
            assert!(is_relative_specifier(specifier), "{specifier}");
            assert!(
                relative_module_rank(importer, specifier, path).is_some(),
                "{importer} {specifier} {path}"
            );
        }
        for specifier in ["a", "@/a", "crate::a", ".a"] {
            assert!(!is_relative_specifier(specifier), "{specifier}");
        }
    }

    /// Probe order: the bare path, the extensions (`.ts` before `.js`), then
    /// the index suffixes; anything else is never probed.
    #[test]
    fn test_relative_module_rank_follows_resolver_probe_order() {
        let rank = |path: &str| relative_module_rank("src/b.ts", "./a", path);
        assert!(rank("src/a").is_some());
        assert!(rank("src/a") < rank("src/a.ts"));
        assert!(rank("src/a.ts") < rank("src/a.js"));
        assert!(rank("src/a.js") < rank("src/a/index.ts"));
        assert!(rank("src/a/index.ts") < rank("src/a/index.js"));
        assert_eq!(rank("src/ab.ts"), None);
        assert_eq!(rank("src/a/other.ts"), None);
        assert_eq!(rank("tests/a.ts"), None);
        assert_eq!(rank("src/__mocks__/a.ts"), None);
        assert!(relative_module_rank("b.ts", "./", "index.ts").is_some());
    }

    #[test]
    fn test_split_qualifier_qualified_callee_takes_segment_before_member() {
        assert_eq!(split_qualifier("K.save"), Some(("K", "save")));
        assert_eq!(split_qualifier("K::st"), Some(("K", "st")));
        assert_eq!(split_qualifier("std::vec::Vec::new"), Some(("Vec", "new")));
        assert_eq!(split_qualifier("\\App\\helper"), Some(("App", "helper")));
        assert_eq!(split_qualifier("super().m"), Some(("super()", "m")));
        for bare in ["save", ".save", "K.", "::st"] {
            assert_eq!(split_qualifier(bare), None, "{bare}");
        }
    }

    #[test]
    fn test_qualifier_prefix_is_internal_external_path_is_rejected() {
        assert!(qualifier_prefix_is_internal("K.save", "K"));
        assert!(qualifier_prefix_is_internal("util::foo", "util"));
        assert!(qualifier_prefix_is_internal("crate::util::foo", "util"));
        assert!(qualifier_prefix_is_internal("super::super::x::foo", "x"));
        assert!(!qualifier_prefix_is_internal("std::fs::read", "fs"));
        assert!(!qualifier_prefix_is_internal("a::b::foo", "b"));
        assert!(!qualifier_prefix_is_internal("save", "save"));
    }

    /// A dirty file's `k.save()` (recorded `K.save`) binds `K`'s member in
    /// the same file, and `K.st()` its static one; an unknown qualifier binds
    /// nothing, though `save` is the only callable of that name.
    #[test]
    fn test_build_qualified_callee_binds_member_in_qualifier_file() {
        let bytes = GraphFixture::new().into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let member = |name: &str, owner: &str| OverlaySymbol {
            kind: NodeKind::Method,
            owner_class: Some(owner.to_string()),
            ..sym(name, &[])
        };
        let class = |name: &str| OverlaySymbol {
            kind: NodeKind::Class,
            ..sym(name, &[])
        };
        let run = OverlaySymbol {
            kind: NodeKind::Method,
            owner_class: Some("App".to_string()),
            ..sym("run", &["K.save", "K.st", "Missing.save"])
        };
        let file = OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: "K.java".into(),
            symbols: vec![
                class("K"),
                member("save", "K"),
                member("st", "K"),
                class("App"),
                run,
            ],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let base = view.base_len();
        let calls: Vec<_> = view
            .overlay_out(base + 4)
            .filter(|(_, e)| e.rel_type == RelType::Calls)
            .map(|(_, e)| (e.target - base, e.confidence))
            .collect();
        assert_eq!(
            calls,
            vec![(1, CONF_QUALIFIER_SCOPED), (2, CONF_QUALIFIER_SCOPED)]
        );
    }

    /// A bare `save()` in `K.run` binds `K.save`, not the interface's
    /// `A.save` that precedes it in the file (`SymbolTable::prefer_owned`).
    #[test]
    fn test_build_same_file_call_from_owner_binds_own_member() {
        let bytes = GraphFixture::new().into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let owned = |kind, name: &str, owner: Option<&str>, calls: &[&str]| OverlaySymbol {
            kind,
            owner_class: owner.map(str::to_string),
            ..sym(name, calls)
        };
        let file = OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: "K.java".into(),
            symbols: vec![
                owned(NodeKind::Interface, "A", None, &[]),
                owned(NodeKind::Method, "save", Some("A"), &[]),
                owned(NodeKind::Class, "K", None, &[]),
                owned(NodeKind::Method, "save", Some("K"), &[]),
                owned(NodeKind::Method, "run", Some("K"), &["save"]),
            ],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let base = view.base_len();
        let calls: Vec<_> = view
            .overlay_out(base + 4)
            .filter(|(_, e)| e.rel_type == RelType::Calls)
            .map(|(_, e)| (e.target - base, e.confidence, e.reason))
            .collect();
        assert_eq!(
            calls,
            vec![(
                3,
                CONF_SAME_FILE,
                crate::session::merged::OVERLAY_EDGE_REASON
            )]
        );
    }

    /// Base copies of one C file under `core/` and `android/core/`, and a
    /// dirty caller of `c_make` at `caller`: the `Calls` edges of its `t`.
    fn nearest_copy_calls(copies: [&str; 2], caller: &str) -> (Vec<u32>, Vec<ViewEdge>) {
        let mut fx = GraphFixture::new();
        let ids = copies.map(|path| fx.func(path, "c_make")).to_vec();
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let file = OverlayFileInput {
            export_aliases: Vec::new(),
            rel_path: caller.into(),
            symbols: vec![sym("t", &["c_make"])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let edges = view
            .overlay_out(view.base_len())
            .map(|(_, e)| *e)
            .filter(|e| e.rel_type == RelType::Calls)
            .collect();
        (ids, edges)
    }

    #[test]
    fn test_build_ambiguous_copies_bind_copy_in_callers_tree() {
        let copies = ["core/c/src/cm.c", "android/core/c/src/cm.c"];
        for (caller, nearest) in [("core/c/test/t.c", 0), ("android/core/c/test/t.c", 1)] {
            let (ids, edges) = nearest_copy_calls(copies, caller);
            let calls: Vec<_> = edges
                .iter()
                .map(|e| (e.target, e.confidence, e.reason))
                .collect();
            assert_eq!(
                calls,
                vec![(ids[nearest], NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON)],
                "{caller}"
            );
        }
    }

    #[test]
    fn test_build_equidistant_copies_bind_nothing() {
        let (_, edges) =
            nearest_copy_calls(["left/c/src/cm.c", "right/c/src/cm.c"], "mid/c/test/t.c");
        assert!(edges.is_empty(), "{edges:?}");
    }

    #[test]
    fn test_build_same_name_in_differently_named_files_binds_nothing() {
        let (_, edges) = nearest_copy_calls(
            ["core/c/src/a.c", "android/core/c/src/b.c"],
            "core/c/test/t.c",
        );
        assert!(edges.is_empty(), "{edges:?}");
    }

    #[test]
    fn test_path_dirs_and_leaf_path_match_index_helpers() {
        assert_eq!(path_dirs("a/b/c.rs").collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(path_dirs("c.rs").count(), 0);
        assert_eq!(leaf_path("a/b/c.rs"), "b/c.rs");
        assert_eq!(leaf_path("b/c.rs"), "b/c.rs");
        assert_eq!(leaf_path("c.rs"), "c.rs");
    }
}
