//! Call resolution for the overlay view: the name tables, `OverlayFacts`,
//! the tier helpers and the edge pass of [`super::OverlayView::build_with_removed`].
//! The view owns assembly, the cache and incremental replay; this module owns
//! how a dirty file's references bind.

use super::{
    base_function_meta, construction_target, emit_closure_references, fragment_emits, reason_index,
    span_of, type_annotation_targets, AnnotatedDecl, CallGap, DirtyNamesake, OverlayFileInput,
    OverlaySymbol, SourceReader, ViewEdge, ViewNode, CS_STRICTEST_FLAGS,
    GAP_CS_ACCESS_DIRTY_TARGET, GAP_CS_EXTENSION_DIRTY_DECLARATION, GAP_CS_PARTIAL_BARE_CALL,
    TYPE_ANNOTATION_REASON,
};
use crate::analyzer::admission::{language_reaches, Gate, Site, SiteForm};
use crate::analyzer::c_family::{
    c_family_field, canonical_declaration, is_c_family, may_be_declaration, CCallable,
};
use crate::analyzer::containment;
use crate::analyzer::csharp_access::{self, CallAccess};
use crate::analyzer::csharp_extension::{self, ReceiverSite};
use crate::analyzer::decorates::{decorates_edges, DECORATES_REASON};
use crate::analyzer::heritage::{
    csharp_base_arity, enclosing_class_heritage, heritage_reason, heritage_rel, holds_heritage,
    HeritageNode,
};
use crate::analyzer::import_binding::{
    fqn_imported_name, fqn_language, fqn_type_path, import_binding, owner_path_agrees,
    DeclaredType, MemberOwner,
};
use crate::analyzer::import_edges::{import_edges, GuessIndex, ImportEdgeFacts};
pub(super) use crate::analyzer::ladder::qualifier_prefix_is_internal;
use crate::analyzer::ladder::{
    self, split_qualifier, CallContext, CallPick, CrateModule, FieldRead, ResolveFacts, StemFile,
    TypeName,
};
use crate::analyzer::lexical::{
    body_locals, decls_enclosing, fold_twins, inside_callable, lexical_chain_end,
    lexical_scope_kinds, twin_enclosing, LexicalFunctions,
};
use crate::analyzer::name_ref::resolve_member_read;
use crate::analyzer::name_tiers::{
    self, import_tiers, name_tiers, HitBarrel, NameTierFacts, TierHit, TierPolicy,
};
use crate::analyzer::outcome::{Resolution, Tier};
use crate::analyzer::overrides::{
    method_lookup, owner_types, rules_for, BaseRow, Decl, MethodIndex, MroCache, OverrideEdges,
    OverrideFacts, OverrideMethod, Rules, MAX_OVERRIDE_DEPTH,
};
use crate::analyzer::path_aliases::PathAliases;
use crate::analyzer::reexport::{
    self, export_filter_applies_to, is_ecmascript, ExportAliasTable, Fact, FileImports, ImportRef,
    ModuleFacts, PyModule, ResolveTarget,
};
use crate::analyzer::rust_paths::{
    is_rust_source, rust_expand_path_head, rust_module_path_base, rust_type_path_prefix,
};
use crate::analyzer::rust_pub_use::{self, PubUseEntries, Stripped};
use crate::analyzer::same_file_pick::{
    prefer_owned, rust_impl_members, ImplMember, ImplNode, PickFacts, PickScope,
};
use crate::analyzer::type_ladder::{
    self, TypeCandidates, TypeFacts, TypeMember as LadderMember, MAX_HERITAGE_DEPTH,
};
use crate::analyzer::types::{
    owner_key, CallArguments, CallSite, MemberRead, NameRef, RawFunctionMeta, RawImport, Span,
    VariantRef, FUNCTION_REFERENCE_REASON, NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON,
    UNTYPED_NAME_CONFIDENCE, UNTYPED_NAME_REASON, VALUE_READ_REASON, VARIANT_PATTERN_REASON,
    VARIANT_USE_REASON,
};
use crate::contracts::identity::lexical_parents_in;
use crate::file_category::{pick_global, FileMeta, GlobalPick, Language};
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, NO_CONTAINER};
use crate::registry::os_path_to_slash;
use crate::session::closure::ConfigEdits;
use crate::session::import_scope::{stored_file_meta, stored_language, ImportScope, Module};
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::hash_map::Entry;
use std::rc::Rc;
use std::sync::Arc;

/// Overlay symbols of one kind family (`kind`), by name: the Tier-3
/// overlay-wide candidates of [`name_tiers()`] ([`OverlayTiers`]), and the
/// names each dirty file declares.
struct OverlayNames<'a> {
    /// The kind family, as the index's lookups name it.
    target: ResolveTarget,
    /// (file ordinal, name) the file declares. Tier 1 picks through
    /// [`OverlayFacts::decls_in_file`]; only the qualifier tier asks here.
    same_file: FxHashSet<(usize, &'a str)>,
    /// name → (virtual idx, file meta), anywhere in the overlay.
    anywhere: FxHashMap<&'a str, Vec<(u32, FileMeta)>>,
    /// The qualifier table only: the Rust `impl` blocks, keyed as
    /// `same_file` and `anywhere` are, for a method-syntax receiver typed as
    /// a type only an `impl` declares ([`ladder::qualifier_file`]).
    impl_same_file: FxHashSet<(usize, &'a str)>,
    impl_anywhere: FxHashMap<&'a str, Vec<(u32, FileMeta)>>,
}

impl<'a> OverlayNames<'a> {
    fn new(target: ResolveTarget) -> Self {
        Self {
            target,
            same_file: FxHashSet::default(),
            anywhere: FxHashMap::default(),
            impl_same_file: FxHashSet::default(),
            impl_anywhere: FxHashMap::default(),
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
        if node.later_overload {
            return;
        }
        let (same_file, anywhere) = if self.target.kind_predicate()(node.kind) {
            (&mut self.same_file, &mut self.anywhere)
        } else if self.target == ResolveTarget::Qualifier && node.kind == NodeKind::Impl {
            (&mut self.impl_same_file, &mut self.impl_anywhere)
        } else {
            return;
        };
        same_file.insert((file_ord, node.name.as_str()));
        if first_of_uid {
            anywhere
                .entry(node.name.as_str())
                .or_default()
                .push((virt, meta));
        }
    }
}

type StarModuleCache = FxHashMap<Box<str>, Rc<[Box<str>]>>;
/// Every input [`OverlayFacts::qualified`] reads that varies within one
/// view: (dirty file ordinal, its imports slice, callee, kind family, the
/// heritage slice when the qualifier is the caller's own type or a Python
/// `super()`, and a Rust caller, whose `impl` block the same-file pick
/// prefers). Whether the Rust module trees run changes only with
/// [`OverlayFacts::tree_free`], which clears the memo.
type QualifiedCache = FxHashMap<
    (
        usize,
        (usize, usize),
        Box<str>,
        ResolveTarget,
        Option<(usize, usize)>,
        Option<u32>,
    ),
    Resolution,
>;
type NamedImportCache =
    FxHashMap<(usize, ResolveTarget), FxHashMap<Box<str>, Option<reexport::NamedImport>>>;
/// Every input of the overlay's Tier 3 ([`OverlayTiers`]) that varies
/// within one view. `owner` and `heritage` hold what the admission gate
/// reads of the site ([`SiteForm::reads_owner`], [`SiteForm::reads_heritage`]).
#[derive(PartialEq, Eq, Hash)]
struct GlobalKey {
    file_ord: usize,
    imports: (usize, usize),
    name: Box<str>,
    target: ResolveTarget,
    /// Whether a call site admits the nearest copy.
    nearest: bool,
    /// Whether a bare call reaches no member.
    free_only: bool,
    owner: Option<Box<str>>,
    heritage: (usize, usize),
    form: SiteForm,
}
type GlobalCache = FxHashMap<GlobalKey, Resolution>;

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
    /// Per dirty file ordinal, the virtual idx of its first symbol.
    dirty_starts: Vec<u32>,
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
    /// File → its imports as [`RawImport`]s, built on first read: a file's
    /// chained calls type their receivers through one callee file's imports.
    raw_imports: RefCell<FxHashMap<String, Rc<[RawImport]>>>,
    /// Module → its resolved `export *` modules, as the index resolves them
    /// once per module.
    star_modules: RefCell<StarModuleCache>,
    /// (dirty file ordinal, kinds) → callee → the named-import tier's answer
    /// ([`OverlayTiers`]): a file calls one imported name at many sites, and
    /// each probe walks the specifier's candidate files. Cleared with
    /// [`Self::qualified`].
    named_imports: RefCell<NamedImportCache>,
    /// [`Self::qualified`]'s answers.
    qualified: RefCell<QualifiedCache>,
    /// Tier 3's answers: a file calls one common name (`get`, `checkNotNull`)
    /// at many sites, and each probe filters every base node of that name.
    global: RefCell<GlobalCache>,
    /// File stem → files, the index's `SymbolTable::files_by_stem`. Built on
    /// the first module-file probe of the qualifier tier: one pass over
    /// `graph.files`, which most builds never pay.
    stems: OnceCell<FxHashMap<&'a str, Vec<&'a str>>>,
    /// Base idx → virtual idx of a surviving dirty-file symbol.
    replaced: &'a FxHashMap<u32, u32>,
    /// First virtual idx of a uid → its later overloads, in source order.
    later_overloads: &'a FxHashMap<u32, Vec<u32>>,
    /// Name → the virtual idxs of the first declaration of each uid, any
    /// kind, no later overload: the overlay's share of the index's global
    /// name table.
    dirty_by_name: FxHashMap<&'a str, Vec<u32>>,
    /// Virtual idx → the Rust `impl` block that declares it, by the index's
    /// rule ([`rust_impl_members`]).
    dirty_impl_members: FxHashMap<u32, ImplMember<'a>>,
    /// Per dirty file: the owner keys of its namespaces and modules that
    /// name no type of the file (the index's `build_owner_kinds`).
    dirty_namespaces: Vec<Vec<&'a str>>,
    /// Clean file index → its Rust `Impl` nodes: span, idx and the trait's
    /// [`owner_key`]. Built on the first `impl`-block question.
    base_impls: OnceCell<FxHashMap<u32, Vec<ImplSpan<'a>>>>,
    /// Clean file index → the owner keys its Namespace / Module nodes carry.
    /// Built on the first namespace-owner question.
    base_namespaces: OnceCell<FxHashMap<u32, Vec<&'a str>>>,
    /// Virtual type → its declared supertypes resolved as the index's
    /// supertypes pre-pass does (`(bases, first_unresolved)`), set once by
    /// [`resolve_edges`] before any call resolves.
    dirty_supertypes: OnceCell<FxHashMap<u32, Supertypes>>,
    /// Rust self-type key → the trait keys of the dirty Rust files' archived
    /// `impl Trait for Type` blocks: the files' old content, whose bases the
    /// archived supertypes of a clean Rust type still carry.
    dirty_impl_traits: FxHashMap<&'a str, Vec<&'a str>>,
    /// Virtual idx → the return type the dirty function declares, as
    /// written ([`OverlaySymbol::return_type`]). Sparse.
    dirty_return_types: FxHashMap<u32, &'a str>,
    /// Virtual idx → the `FunctionMeta::flags` the dirty method's own parse
    /// carries ([`OverlaySymbol::flags`]). Sparse: a method with no meta is
    /// absent, and its flags are unknown.
    dirty_flags: FxHashMap<u32, u16>,
    /// Scoped virtual idx → its lexical parent's virtual idx, from the
    /// fresh parse as the index derives its table. A virtual node has no
    /// row in the graph's table. Sparse: empty for a dirty file whose
    /// language scopes no kind ([`lexical_scope_kinds`]).
    dirty_lexical_parents: FxHashMap<u32, u32>,
    /// C# file → the namespaces it declares (`.`-joined), the index's
    /// `SymbolTable::csharp_namespaces`. Built on the first C# extension
    /// question.
    csharp_namespaces: OnceCell<FxHashMap<&'a str, Vec<&'a str>>>,
    /// Per virtual offset, the dirty symbol's container as a virtual idx, or
    /// `NO_CONTAINER` at file level: [`containment::containers`] over each
    /// dirty file's symbols, as the index computes `ZeroCopyGraph::containers`
    /// over its parse.
    dirty_containers: Vec<u32>,
    /// The Rust module trees ([`ladder::ModuleTreeFacts`]).
    rust: RustModules<'a>,
    /// Dirty file ordinal → where it stands in [`Self::rust`], read on its
    /// first module-tree question.
    rust_callers: RefCell<FxHashMap<usize, Rc<RustCaller<'a>>>>,
    /// Set for the `Imports` pass ([`resolve_imports`]): candidate files are
    /// read without the Rust module trees, as the index's import resolver
    /// has none (`GraphBuilder::build`'s `imports_resolver`).
    tree_free: Cell<bool>,
}

/// A base Rust `Impl` node: its span, its idx and the [`owner_key`] of the
/// trait it implements.
/// A type's resolved bases in declared order, and its first unresolved one.
type Supertypes = (Vec<u32>, Option<u32>);
type ImplSpan<'a> = ((u32, u32, u32, u32), u32, Option<&'a str>);

/// The span of the stored node `node`, as its parse wrote it.
fn stored_span(node: &crate::graph::ArchivedNode) -> Span {
    let span = &node.span;
    (
        span.0.to_native(),
        span.1.to_native(),
        span.2.to_native(),
        span.3.to_native(),
    )
}

/// Whether the base file `file_idx` is a Rust source file.
fn is_rust_file(graph: &ArchivedZeroCopyGraph, file_idx: usize) -> bool {
    graph
        .files
        .get(file_idx)
        .is_some_and(|f| is_rust_source(std::path::Path::new(f.path.resolve(&graph.string_pool))))
}

/// Per base file index, the spans of its callables when it is a Rust file,
/// none otherwise: one path test per file, one index per callable.
fn base_rust_callables(graph: &ArchivedZeroCopyGraph) -> Vec<Vec<Span>> {
    let rust: Vec<bool> = (0..graph.files.len())
        .map(|file_idx| is_rust_file(graph, file_idx))
        .collect();
    let mut callables = vec![Vec::new(); rust.len()];
    for kind in NodeKind::ALL.into_iter().filter(|kind| kind.is_callable()) {
        for idx in graph.nodes_by_kind(kind) {
            let callable = &graph.nodes[idx as usize];
            let file_idx = callable.file_idx.to_native() as usize;
            if rust.get(file_idx).copied().unwrap_or(false) {
                callables[file_idx].push(stored_span(callable));
            }
        }
    }
    callables
}

/// What [`RustModules`] reads about the dirty files' symbols, derived once
/// per view in `OverlayFacts::new`. Virtual idxs number the dirty files'
/// symbols in order after the graph's nodes.
#[derive(Debug, Default)]
pub(super) struct DirtyRustFacts {
    /// Per dirty file, in input order, the virtual idx of its first symbol.
    starts: Vec<u32>,
    /// The declarations no module path names: the scoped ones, and the
    /// Rust ones inside a function body ([`body_locals`]), where a first of
    /// a uid with a module-level twin counts as a module item.
    body_locals: FxHashSet<u32>,
    /// The later twins of a uid, which the index tombstones into the first:
    /// no name lookup reads them, and no module path names them. A later
    /// node (`ViewNode::later_overload`) is no twin: it keeps a node of its
    /// own.
    repeats: FxHashSet<u32>,
}

impl<'a> OverlayFacts<'a> {
    /// Drop the qualified and named-import answers, which read facts a later
    /// pass changes: the candidate files the module trees place, which the
    /// `Imports` and decorator passes no longer read
    /// ([`Self::tree_free`]), and the dirty bases, which the same-file
    /// pick's hierarchy rule and the named-import tier's member pick walk
    /// once the pre-pass made them. Tier 3's memo reads neither and stays.
    fn forget_tree_answers(&self) {
        self.qualified.borrow_mut().clear();
        self.named_imports.borrow_mut().clear();
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        graph: &'a ArchivedZeroCopyGraph,
        files: &'a [OverlayFileInput],
        nodes: &'a [ViewNode],
        dirty_base: &'a FxHashSet<u32>,
        removed: &'a FxHashSet<&'a str>,
        replaced: &'a FxHashMap<u32, u32>,
        later_overloads: &'a FxHashMap<u32, Vec<u32>>,
        configs: &ConfigEdits,
        sources: Option<SourceReader<'a>>,
    ) -> Self {
        let base_len = graph.nodes.len() as u32;
        let mut declared: FxHashMap<(usize, &'a str), Vec<u32>> = FxHashMap::default();
        let mut dirty_by_name: FxHashMap<&'a str, Vec<u32>> = FxHashMap::default();
        let mut dirty_impl_members: FxHashMap<u32, ImplMember<'a>> = FxHashMap::default();
        let mut dirty_namespaces: Vec<Vec<&'a str>> = Vec::with_capacity(files.len());
        let mut exported = Vec::with_capacity(nodes.len());
        let mut es_modules = Vec::with_capacity(files.len());
        let mut dirty_aliases = Vec::with_capacity(files.len());
        let mut dirty_return_types: FxHashMap<u32, &'a str> = FxHashMap::default();
        let mut dirty_flags: FxHashMap<u32, u16> = FxHashMap::default();
        let mut dirty_lexical_parents: FxHashMap<u32, u32> = FxHashMap::default();
        let mut dirty_containers: Vec<u32> = Vec::with_capacity(nodes.len());
        let mut rust_facts = DirtyRustFacts::default();
        // The first virtual idx of each uid: the index merges each repeat
        // that carries it into it, a later node included.
        let mut first_of_uid: FxHashMap<u64, u32> = FxHashMap::default();
        // One Rust file's twins as (offset, offset of its first); a uid
        // hashes the path, so both lie in the file.
        let mut file_twins: Vec<(usize, usize)> = Vec::new();
        let mut offset = 0usize;
        for (file_ord, file) in files.iter().enumerate() {
            let mut exports_any = false;
            let first = offset;
            let start = base_len + first as u32;
            let rust_file = is_rust_source(std::path::Path::new(&file.rel_path));
            let csharp_file = Language::from_normalized_path(&file.rel_path) == Language::CSharp;
            for symbol in &file.symbols {
                let node = &nodes[offset];
                let virt = base_len + offset as u32;
                // A later node is a declaration of its own in no name table,
                // and the survivor of a repeat merged into it (a C# arity
                // sibling's): that repeat, under its uid, stays out too.
                if node.later_overload {
                    first_of_uid.entry(node.uid).or_insert(virt);
                } else {
                    match first_of_uid.entry(node.uid) {
                        Entry::Occupied(seen) => {
                            rust_facts.repeats.insert(virt);
                            if let Some(first_at) =
                                seen.get().checked_sub(start).filter(|_| rust_file)
                            {
                                file_twins.push((offset - first, first_at as usize));
                            }
                        }
                        Entry::Vacant(slot) => {
                            slot.insert(virt);
                            declared
                                .entry((file_ord, node.name.as_str()))
                                .or_default()
                                .push(virt);
                            dirty_by_name
                                .entry(node.name.as_str())
                                .or_default()
                                .push(virt);
                            exports_any |= symbol.is_exported;
                        }
                    }
                }
                exported.push(symbol.is_exported);
                // As the index's `pass1_8_function_metas`: a C# field or
                // property types a receiver by its declared type, which
                // another `partial` part reads.
                let return_type = match (symbol.return_type.as_deref(), symbol.kind) {
                    (None, NodeKind::Property) if csharp_file => symbol.type_annotation.as_deref(),
                    (return_type, _) => return_type,
                };
                if let Some(return_type) = return_type {
                    dirty_return_types.insert(base_len + offset as u32, return_type);
                }
                if let Some(flags) = symbol.flags {
                    dirty_flags.insert(base_len + offset as u32, flags);
                }
                offset += 1;
            }
            rust_facts.starts.push(start);
            let rows: Vec<containment::Row<'_>> = file
                .symbols
                .iter()
                .map(|s| containment::Row {
                    kind: s.kind,
                    owner: s.owner_class.as_deref(),
                    name: &s.name,
                    span: HeritageNode::span(s),
                })
                .collect();
            dirty_containers.extend(
                containment::containers(&rows)
                    .into_iter()
                    .map(|c| c.map_or(NO_CONTAINER, |raw| start + raw as u32)),
            );
            if rust_file {
                rust_impl_members(&file.symbols, |member, block, implemented| {
                    dirty_impl_members.insert(
                        start + member as u32,
                        ImplMember {
                            block: start + block as u32,
                            trait_owner: implemented.and_then(owner_key),
                        },
                    );
                });
            }
            let scoped = lexical_scope_kinds(Language::from_path(&file.rel_path));
            if rust_file || !scoped.is_empty() {
                // The fields `file_identities` numbers the file from.
                let decls: Vec<(NodeKind, Span)> = file
                    .symbols
                    .iter()
                    .map(|s| (s.kind, HeritageNode::span(s)))
                    .collect();
                let enclosing = decls_enclosing(&decls);
                let parents = lexical_parents_in(&decls, &enclosing, scoped);
                for (offset, parent) in parents.into_iter().enumerate() {
                    if let Some(parent) = parent {
                        let virt = start + offset as u32;
                        dirty_lexical_parents.insert(virt, start + parent);
                        rust_facts.body_locals.insert(virt);
                    }
                }
                if rust_file {
                    let mut local = body_locals(&decls, &enclosing);
                    fold_twins(&mut local, file_twins.drain(..));
                    rust_facts.body_locals.extend(
                        local
                            .iter()
                            .enumerate()
                            .filter(|&(_, &body_local)| body_local)
                            .map(|(offset, _)| start + offset as u32),
                    );
                }
            }
            dirty_namespaces.push(namespace_owners(&file.symbols));
            let aliases = ExportAliasTable::build(alias_pairs(file));
            es_modules.push(reexport::is_es_module(
                file.file_meta().language,
                &file.rel_path,
                exports_any,
                aliases.is_some(),
            ));
            dirty_aliases.push(aliases);
        }
        let pool = &graph.string_pool;
        let mut dirty_impl_traits: FxHashMap<&'a str, Vec<&'a str>> = FxHashMap::default();
        for &idx in dirty_base {
            let block = &graph.nodes[idx as usize];
            // The kind test first: it is cheap, and few dirty nodes are `Impl`.
            if NodeKind::from(&block.kind) != NodeKind::Impl {
                continue;
            }
            let file_idx = block.file_idx.to_native();
            let rust_file = graph.files.get(file_idx as usize).is_some_and(|f| {
                stored_language(graph, file_idx, f.path.resolve(pool)) == Language::Rust
            });
            if !rust_file {
                continue;
            }
            let (Some(ty), Some(implemented)) = (
                owner_key(block.name.resolve(pool)),
                owner_key(block.owner_class.resolve(pool)),
            ) else {
                continue;
            };
            dirty_impl_traits.entry(ty).or_default().push(implemented);
        }
        // The dirty configs' rows over the archived table, merged once per
        // view: the alias reading and the crate table both read it.
        let merged = configs.merged_facts(graph);
        let dirty_starts = rust_facts.starts.clone();
        let rust = RustModules::new(graph, files, removed, rust_facts).with_sources(sources);
        if let Some(facts) = &merged {
            let _ = rust.crates.set(facts.clone());
        }
        Self {
            graph,
            scope: ImportScope::new(graph, files, dirty_base, removed).with_aliases(
                overlay_aliases(graph, configs.repo_wide_aliases(), merged.as_ref()),
            ),
            nodes,
            dirty_base,
            dirty: files
                .iter()
                .enumerate()
                .map(|(file_ord, file)| (file.rel_path.as_str(), file_ord))
                .collect(),
            dirty_starts,
            declared,
            exported,
            es_modules,
            dirty_aliases,
            clean_aliases: RefCell::default(),
            raw_imports: RefCell::default(),
            star_modules: RefCell::default(),
            named_imports: RefCell::default(),
            qualified: RefCell::default(),
            global: RefCell::default(),
            stems: OnceCell::new(),
            replaced,
            later_overloads,
            dirty_by_name,
            dirty_impl_members,
            dirty_namespaces,
            base_impls: OnceCell::new(),
            base_namespaces: OnceCell::new(),
            dirty_supertypes: OnceCell::new(),
            dirty_impl_traits,
            dirty_return_types,
            dirty_flags,
            dirty_lexical_parents,
            dirty_containers,
            csharp_namespaces: OnceCell::new(),
            rust,
            rust_callers: RefCell::default(),
            tree_free: Cell::new(false),
        }
    }

    /// `f` over the module trees as the dirty file `file_ord` reads them.
    fn with_rust_tree<R>(&self, file_ord: usize, f: impl FnOnce(&OverlayTree<'_, 'a>) -> R) -> R {
        let cached = self.rust_callers.borrow().get(&file_ord).cloned();
        let caller = self.scope.dirty()[file_ord].rel_path.as_str();
        let position = cached.unwrap_or_else(|| {
            let position = Rc::new(self.rust.caller(caller));
            self.rust_callers
                .borrow_mut()
                .insert(file_ord, Rc::clone(&position));
            position
        });
        f(&OverlayTree {
            modules: &self.rust,
            caller,
            position: &position,
        })
    }

    /// Whether `idx` is scoped: a virtual node by the dirty parse, a base
    /// node by the graph's lexical-parent table.
    fn is_lexically_scoped(&self, idx: u32) -> bool {
        if idx >= self.base_len() {
            self.dirty_lexical_parents.contains_key(&idx)
        } else {
            self.graph.is_lexically_scoped(idx)
        }
    }

    /// The outermost unscoped declaration the dirty parse's lexical
    /// parents reach from the virtual `idx`; `idx` itself when it is
    /// unscoped. The index's `SymbolTable::lexical_root`, cycle rule
    /// included.
    fn lexical_root(&self, idx: u32) -> u32 {
        lexical_chain_end(idx, self.dirty_lexical_parents.len(), |idx| {
            self.dirty_lexical_parents.get(&idx).copied()
        })
        .unwrap_or(idx)
    }

    fn base_len(&self) -> u32 {
        self.graph.nodes.len() as u32
    }

    /// The declared name of `idx` (base or virtual).
    fn name_of(&self, idx: u32) -> &'a str {
        let (graph, nodes) = (self.graph, self.nodes);
        match idx.checked_sub(self.base_len()) {
            Some(offset) => nodes[offset as usize].name.as_str(),
            None => graph.nodes[idx as usize].name.resolve(&graph.string_pool),
        }
    }

    /// The index's `SymbolTable::enum_variant_of`: the `EnumVariant` named
    /// `variant` in the file of the type `type_idx` whose owner is the type,
    /// by the name the reference used (`type_name`, an import alias
    /// included), else by the type's declared name.
    fn enum_variant_of(&self, type_idx: u32, type_name: &str, variant: &str) -> Option<u32> {
        let file = self.node_file(type_idx)?;
        let decls = self.decls_in_file(file, variant);
        let owned_by = |owner: &str| {
            let key = owner_key(owner)?;
            decls
                .iter()
                .find(|&&(idx, kind)| {
                    kind == NodeKind::EnumVariant && self.owner_of(idx) == Some(key)
                })
                .map(|&(idx, _)| idx)
        };
        owned_by(type_name).or_else(|| owned_by(self.name_of(type_idx)))
    }

    /// The file of the `crate` / `self` / `super`-anchored Rust module
    /// `module_path` that the module trees of every target declaring `source`
    /// agree on ([`ladder::anchored_module_file`]); `None` for a path the
    /// trees do not place, and for one they place at different files.
    fn anchored_rust_module(&self, source: &str, module_path: &str) -> Option<&'a str> {
        if !matches!(
            module_path.split("::").next(),
            Some("crate" | "self" | "super")
        ) || !is_rust_source(std::path::Path::new(source))
        {
            return None;
        }
        match self.dirty.get(source) {
            Some(&file_ord) => self.with_rust_tree(file_ord, |tree| {
                ladder::anchored_module_file(tree, source, module_path)
            }),
            None => {
                let position = self.rust.caller(source);
                let tree = OverlayTree {
                    modules: &self.rust,
                    caller: source,
                    position: &position,
                };
                ladder::anchored_module_file(&tree, source, module_path)
            }
        }
    }

    /// The index's `Resolver::rust_module_file`: the file of the anchored
    /// Rust module `module_path` the trees place ([`Self::anchored_rust_module`]),
    /// else the one by conventional layout.
    fn rust_module_file(&self, source: &str, module_path: &str) -> Option<&'a str> {
        if let Some(file) = self.anchored_rust_module(source, module_path) {
            return self.has_file(file).then_some(file);
        }
        let base = rust_module_path_base(std::path::Path::new(source), module_path)?;
        let base = os_path_to_slash(&base);
        let mut found = None;
        reexport::probe_rust_module(base.trim_start_matches("./"), &mut |candidate: &str| {
            found = self.stored_path(candidate);
            found.is_none()
        });
        found
    }

    /// The index's `Resolver::rust_type_path_agrees`: may `qualifier` of the
    /// Rust path call `callee` (`a::b::Q::m`) be the type declared in
    /// `candidate`? The module tree places `Q` ([`ladder::module_tree_item`]);
    /// `None` where it meets a `use` the overlay cannot see through, or a
    /// module file that does not declare `Q` itself.
    fn rust_type_path_agrees(
        &self,
        (file_ord, source): (usize, &str),
        callee: Option<&str>,
        qualifier: &str,
        candidate: &str,
        imports: &[RawImport],
        qualifiers: &OverlayNames<'_>,
    ) -> Option<bool> {
        let (Some(full), true) = (callee, is_rust_source(std::path::Path::new(source))) else {
            return Some(true);
        };
        let Some(prefix) = rust_type_path_prefix(full, qualifier) else {
            return Some(true);
        };
        if qualifier_prefix_is_internal(full, qualifier) {
            return Some(true);
        }
        let named = rust_expand_path_head(prefix, imports);
        let declared_here = |module: &str| qualifiers.same_file.contains(&(file_ord, module));
        let tree_item = |path: String| {
            self.with_rust_tree(file_ord, |tree| {
                ladder::module_tree_item(tree, source, &path, ladder::Namespace::Type)
            })
        };
        let module_path = match named[0] {
            "crate" | "self" | "super" => named.join("::"),
            head if declared_here(head) => format!("self::{}", named.join("::")),
            _ => {
                // Only a workspace crate's module is left; the tree places
                // `Q` itself, through its `pub use` chain.
                return match tree_item(format!("{}::{qualifier}", named.join("::"))) {
                    Some(ladder::TreeItem::Found { file, .. }) => Some(file == candidate),
                    Some(ladder::TreeItem::Unknown) => None,
                    None => Some(false),
                };
            }
        };
        // The module may only re-export `Q` (`mod lock; pub use
        // lock::FileLock;`): the tree follows that `pub use` chain to the
        // defining file, which the module's own file is not.
        match tree_item(format!("{module_path}::{qualifier}")) {
            Some(ladder::TreeItem::Found { file, item }) if item == qualifier => {
                return Some(file == candidate);
            }
            Some(ladder::TreeItem::Unknown) => return None,
            _ => {}
        }
        if let Some(module_file) = self.rust_module_file(source, &module_path) {
            if module_file == candidate {
                return Some(true);
            }
            return self
                .first_in_file(module_file, qualifier, ResolveTarget::Qualifier, None)
                .map(|_| false);
        }
        Some(
            candidate == source
                && named
                    .iter()
                    .filter(|seg| !matches!(**seg, "crate" | "self" | "super"))
                    .all(|&seg| declared_here(seg)),
        )
    }

    /// The `File` node of `path` for an `Imports` edge: a dirty path's from
    /// `dirty_files` (the replay's numbering), a clean one's last base `File`
    /// node, as the index's `file_node_idx` keeps the last. `None` for a
    /// removed path and a path no file has.
    fn file_node(&self, dirty_files: &FxHashMap<&str, u32>, path: &str) -> Option<u32> {
        if let Some(&node) = dirty_files.get(path) {
            return Some(node);
        }
        if self.dirty.contains_key(path) || self.scope.is_removed(path) {
            return None;
        }
        let graph = self.graph;
        let basename = path.rsplit('/').next().unwrap_or(path);
        graph
            .nodes_by_name(basename)
            .filter(|&idx| {
                let node = &graph.nodes[idx as usize];
                NodeKind::from(&node.kind) == NodeKind::File
                    && graph
                        .files
                        .get(node.file_idx.to_native() as usize)
                        .is_some_and(|f| f.path.resolve(&graph.string_pool) == path)
            })
            .max()
    }

    /// Every path with a `File` node and that node, each path once: the clean
    /// base paths, then `dirty_files`. One pass over the base `File` nodes.
    fn file_nodes(&self, dirty_files: &FxHashMap<&'a str, u32>) -> Vec<(&'a str, u32)> {
        let graph = self.graph;
        let mut last: FxHashMap<&'a str, u32> = FxHashMap::default();
        for idx in graph.nodes_by_kind(NodeKind::File) {
            let Some(file) = graph
                .files
                .get(graph.nodes[idx as usize].file_idx.to_native() as usize)
            else {
                continue;
            };
            let path = file.path.resolve(&graph.string_pool);
            if self.dirty.contains_key(path) || self.scope.is_removed(path) {
                continue;
            }
            let node = last.entry(path).or_insert(idx);
            *node = (*node).max(idx);
        }
        last.into_iter()
            .chain(dirty_files.iter().map(|(&path, &node)| (path, node)))
            .collect()
    }

    /// The stored spelling of `file`, a dirty or an indexed path. `None` for
    /// an indexed path the worktree removed.
    /// The base file index of a clean `file`. `None` for a path the worktree
    /// no longer has, so no fact reads a removed file's declarations.
    fn clean_file_idx(&self, file: &str) -> Option<u32> {
        if self.scope.is_removed(file) {
            return None;
        }
        self.graph.file_idx_of(file)
    }

    fn stored_path(&self, file: &str) -> Option<&'a str> {
        if let Some((&path, _)) = self.dirty.get_key_value(file) {
            return Some(path);
        }
        let graph = self.graph;
        let file_idx = self.clean_file_idx(file)?;
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
                .same_namespaces(self.meta_of(a).language, a_file, b_file)
    }

    /// The index's `SymbolTable::nearest_copy`: among `copies`, copies of
    /// one declaration, the one whose file shares the deepest directory with
    /// `caller_file`, fewer directory steps breaking an equal depth. `None`
    /// for a tie, for fewer than two, or when one is no copy of the first.
    fn nearest_of_copies(&self, caller_file: &str, copies: &[u32]) -> Option<u32> {
        let (&first, rest) = copies.split_first()?;
        if rest.is_empty() || !rest.iter().all(|&idx| self.is_copy(first, idx)) {
            return None;
        }
        let caller_dirs = || path_dirs(caller_file);
        let mut best: Option<((usize, std::cmp::Reverse<usize>), u32)> = None;
        let mut tie = false;
        for &idx in copies {
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
    /// such a file here can make a stem ambiguous, never add a target. A
    /// removed path is no file.
    fn files_by_stem(&self, stem: &str) -> &[&'a str] {
        let stems = self.stems.get_or_init(|| {
            let graph = self.graph;
            let base = graph
                .files
                .iter()
                .map(|f| f.path.resolve(&graph.string_pool))
                .filter(|path| !self.dirty.contains_key(path) && !self.scope.is_removed(path));
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

    /// [`ladder::qualified`] for a qualified callee of the dirty file
    /// `file_ord`, memoized: a file types one receiver at many sites
    /// (`segment.get()`, `K.m()`), and each probe walks the qualifier's
    /// files and the same-file pick. `module_tree` is the overlay's module
    /// tree lookup of the callee ([`OverlayTiers`]).
    fn qualified(
        &self,
        file_ord: usize,
        site: ladder::QualifiedSite<'_>,
        qualifiers: &OverlayNames<'_>,
        heritage: &[String],
        module_tree: impl FnOnce() -> Option<u32>,
    ) -> Resolution {
        let caller_type = site
            .call
            .and_then(|call| call.owner)
            .and_then(owner_key)
            .is_some_and(|owner| owner_key(site.qualifier) == Some(owner));
        let reads_heritage = caller_type
            || (site.qualifier == CallSite::SUPER_RECEIVER
                && site.caller.language == Language::Python);
        let key = (
            file_ord,
            (site.imports.as_ptr() as usize, site.imports.len()),
            Box::<str>::from(site.callee),
            site.target,
            reads_heritage.then_some((heritage.as_ptr() as usize, heritage.len())),
            site.call
                .and_then(|call| call.caller)
                .filter(|_| site.caller.language == Language::Rust),
        );
        if let Some(&hit) = self.qualified.borrow().get(&key) {
            return hit;
        }
        let (hit, _) = ladder::qualified(
            &OverlayQualifiers {
                facts: self,
                file_ord,
                qualifiers,
            },
            site,
            heritage,
            StemFile::DeclaringMember,
            module_tree,
        );
        self.qualified.borrow_mut().insert(key, hit);
        hit
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

    fn declares_top_level_module(&self, file: &str, name: &str) -> bool {
        self.decls_in_file(file, name)
            .into_iter()
            .any(|(id, kind)| kind == NodeKind::Module && PickFacts::owner(self, id).is_none())
    }

    fn has_file(&self, file: &str) -> bool {
        self.dirty.contains_key(file) || self.clean_file_idx(file).is_some()
    }

    fn file_language(&self, file: &str) -> Language {
        self.scope.file_meta(file).language
    }

    fn has_module_name(&self, language: Language, name: &str) -> bool {
        self.scope.has_module_name(language, name)
    }

    fn namespace_files(&self, language: Language, namespace: &str) -> Vec<Self::Path> {
        let mut files: Vec<_> = self
            .global_named(namespace)
            .into_iter()
            .filter(|&id| {
                self.kind_of(id) == NodeKind::Namespace && self.meta_of(id).language == language
            })
            .filter_map(|id| self.node_file(id))
            .collect();
        files.sort_unstable();
        files.dedup();
        files
    }

    fn files_in_directory(&self, language: Language, directory: &str) -> Vec<Self::Path> {
        self.scope.files_in_directory(language, directory)
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
                        .filter(|&&virt| !self.dirty_lexical_parents.contains_key(&virt))
                        .map(|&virt| (virt, self.nodes[(virt - base_len) as usize].kind))
                        .collect()
                });
        }
        let Some(file_idx) = self.clean_file_idx(file) else {
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
                    && !self.graph.is_lexically_scoped(idx)
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
        if !export_filter_applies_to(self.scope.file_meta(file).language, file) {
            return None;
        }
        if let Some(&file_ord) = self.dirty.get(file) {
            return self.dirty_aliases[file_ord].as_ref().map(Fact::Borrowed);
        }
        let file_idx = self.clean_file_idx(file)?;
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
        let rows = self.graph.file_imports(self.clean_file_idx(file)?)?;
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
        // The index's `for_each_candidate`: a Rust path the module trees
        // place is that one file; any other goes through
        // `for_each_specifier_candidate` with its path aliases, read in the
        // importer's scope. The `Imports` pass reads no tree, as the index's.
        if let Some(file) = (!self.tree_free.get())
            .then(|| self.anchored_rust_module(importer, specifier))
            .flatten()
        {
            visit(file);
            return;
        }
        let aliases = self.scope.aliases();
        reexport::for_each_specifier_candidate(
            std::path::Path::new(importer),
            specifier,
            |expanded| {
                if !aliases.is_empty() {
                    let language = self.scope.file_meta(importer).language;
                    aliases.expand_in(aliases.scope_of(importer, language), specifier, |alias| {
                        expanded(alias)
                    });
                }
            },
            visit,
        );
    }
}

impl<'a> OverlayFacts<'a> {
    /// The clean base declarations named `name` that the index's name
    /// tables hold (no dirty-file node, no later overload), ascending.
    fn clean_named(&self, name: &str) -> Vec<u32> {
        let graph = self.graph;
        let mut ids: Vec<u32> = graph
            .nodes_by_name(name)
            .filter(|&idx| {
                !self.dirty_base.contains(&idx)
                    && !graph.is_later_overload(idx)
                    && fragment_emits(NodeKind::from(&graph.nodes[idx as usize].kind))
            })
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Every declaration named `name` the index's global name table holds:
    /// the clean base ones, then the overlay's first of each uid.
    fn global_named(&self, name: &str) -> Vec<u32> {
        let mut ids = self.clean_named(name);
        ids.extend(self.dirty_by_name.get(name).into_iter().flatten());
        ids
    }

    /// The [`FileMeta`] of the file `idx` is declared in, as the index reads
    /// it: the fresh parse's language for a dirty file (a virtual node, or a
    /// base node of a re-parsed path), else the language `graph.bin` stores.
    fn meta_of(&self, idx: u32) -> FileMeta {
        match idx.checked_sub(self.base_len()) {
            Some(offset) => self.scope.file_meta(&self.nodes[offset as usize].rel_path),
            None if self.dirty_base.contains(&idx) => self
                .node_file(idx)
                .map_or_else(FileMeta::default, |file| self.scope.file_meta(file)),
            None => stored_file_meta(
                self.graph,
                self.graph.nodes[idx as usize].file_idx.to_native(),
            ),
        }
    }

    /// The later overloads of the first declaration `first`, in source order
    /// (the index's `SymbolTable::later_overloads`).
    fn later_overloads_of(&self, first: u32) -> Vec<u32> {
        if first >= self.base_len() {
            return self
                .later_overloads
                .get(&first)
                .cloned()
                .unwrap_or_default();
        }
        // The index stores each set as `overload_set` References from the
        // first. A name scan over `later_nodes` cannot tell an overload from
        // a C# arity sibling, which joins no set.
        let graph = self.graph;
        let out = &graph.out_offsets;
        let (start, end) = (
            out[first as usize].to_native() as usize,
            out[first as usize + 1].to_native() as usize,
        );
        let mut later: Vec<u32> = graph.edges[start..end]
            .iter()
            .filter(|edge| {
                RelType::from(&edge.rel_type) == RelType::References
                    && edge.reason.resolve(&graph.string_pool)
                        == crate::analyzer::types::OVERLOAD_SET_REASON
            })
            .map(|edge| edge.target.to_native())
            .collect();
        later.sort_unstable();
        later
    }

    /// Clean file index → its Rust `Impl` nodes: span, idx and the trait's
    /// [`owner_key`]. Built on the first `impl`-block question.
    fn base_impls(&self) -> &FxHashMap<u32, Vec<ImplSpan<'a>>> {
        let graph = self.graph;
        self.base_impls.get_or_init(|| {
            let mut impls: FxHashMap<u32, Vec<ImplSpan<'a>>> = FxHashMap::default();
            for impl_idx in graph.nodes_by_kind(NodeKind::Impl) {
                let block = &graph.nodes[impl_idx as usize];
                let file_idx = block.file_idx.to_native();
                if !is_rust_file(graph, file_idx as usize) || self.dirty_base.contains(&impl_idx) {
                    continue;
                }
                impls.entry(file_idx).or_default().push((
                    stored_span(block),
                    impl_idx,
                    owner_key(block.owner_class.resolve(&graph.string_pool)),
                ));
            }
            impls
        })
    }

    /// The `impl` block of the clean base Rust method `idx`: the innermost
    /// `Impl` node of its file whose span holds it. A method of an `impl`
    /// over a self type the parser emits no `Impl` node for (`impl T for
    /// &mut X`) has none here: `graph.bin` keeps no method heritage, which
    /// the index groups such methods by.
    fn base_impl_member(&self, idx: u32) -> Option<ImplMember<'a>> {
        let graph = self.graph;
        let node = &graph.nodes[idx as usize];
        if !matches!(
            NodeKind::from(&node.kind),
            NodeKind::Method | NodeKind::Constructor
        ) {
            return None;
        }
        let impls = self.base_impls();
        let span = (
            node.span.0.to_native(),
            node.span.1.to_native(),
            node.span.2.to_native(),
            node.span.3.to_native(),
        );
        impls
            .get(&node.file_idx.to_native())?
            .iter()
            .filter(|&&(outer, _, _)| {
                (outer.0, outer.1) <= (span.0, span.1) && (outer.2, outer.3) >= (span.2, span.3)
            })
            .min_by_key(|&&(outer, _, _)| {
                (
                    outer.2.saturating_sub(outer.0),
                    outer.3.saturating_sub(outer.1),
                )
            })
            .map(|&(_, block, trait_owner)| ImplMember { block, trait_owner })
    }

    /// The index's resolved supertypes of a base type, with a base the edit
    /// replaced read as its virtual node, and one the edit deleted or
    /// renamed read as unresolved where it stood. A trait only a dirty
    /// file's archived `impl` block gave the type is gone: the pre-pass adds
    /// back what the file's fresh parse still implements.
    fn base_supertypes(&self, ty: u32) -> Option<Supertypes> {
        let (bases, archived_unresolved) = self.graph.supertypes_of(ty)?;
        // The table keys Rust types by name alone, so a Python or Java
        // namesake of a Rust self type keeps every base its own file gave.
        let stale = (self.meta_of(ty).language == Language::Rust)
            .then(|| owner_key(self.name_of(ty)))
            .flatten()
            .and_then(|key| self.dirty_impl_traits.get(key));
        let mut first_unresolved = None;
        let mut kept = Vec::with_capacity(bases.len());
        for (at, base) in bases.into_iter().enumerate() {
            if archived_unresolved == Some(at as u32) {
                first_unresolved.get_or_insert(kept.len() as u32);
            }
            if stale.is_some_and(|traits| self.only_dirty_impl_gives(ty, base, traits)) {
                continue;
            }
            if let Some(&virt) = self.replaced.get(&base) {
                kept.push(virt);
            } else if self.dirty_base.contains(&base) {
                first_unresolved.get_or_insert(kept.len() as u32);
            } else {
                kept.push(base);
            }
        }
        if archived_unresolved.is_some() {
            first_unresolved.get_or_insert(kept.len() as u32);
        }
        Some((kept, first_unresolved))
    }

    /// The index's `SymbolTable::partial_parts` for base or virtual `id`:
    /// the parts of its C# `partial` type, the representative first, read
    /// from the archived `PartOf` edges ([`ArchivedZeroCopyGraph::partial_parts`])
    /// with a part the edit replaced read as its virtual node. The overlay
    /// has no project identity, so it keeps the last index's grouping: a
    /// part an edit added stays a type of its own. Empty when `id` is no
    /// part, or when the edit deleted the representative or every other part.
    fn partial_group(&self, id: u32) -> Vec<u32> {
        // Only a C# type has parts; the test spares every other candidate
        // the edge scan.
        let csharp = self.meta_of(id).language == Language::CSharp;
        if !csharp {
            return Vec::new();
        }
        let base = match id.checked_sub(self.base_len()) {
            Some(offset) => self
                .nodes
                .get(offset as usize)
                .and_then(|n| n.replaced_base),
            None => Some(id),
        };
        let Some(base) = base else {
            return Vec::new();
        };
        let current = |part: u32| match self.replaced.get(&part) {
            Some(&virt) => Some(virt),
            None => (!self.dirty_base.contains(&part)).then_some(part),
        };
        let parts = self.graph.partial_parts(base);
        let Some(representative) = parts.first().and_then(|&rep| current(rep)) else {
            return Vec::new();
        };
        let others: Vec<u32> = parts[1..]
            .iter()
            .filter_map(|&part| current(part))
            .collect();
        if others.is_empty() {
            return Vec::new();
        }
        std::iter::once(representative).chain(others).collect()
    }

    /// The caller's own type for the partial-type tier, with its
    /// [`owner_key`]: the type `owner` that `file` declares, when the call
    /// is a bare C# call `name()` of a callable. The conditions of the
    /// index's `Resolver::partial_type_member`.
    fn partial_caller_type<'o>(
        &self,
        file: &str,
        name: &str,
        owner: &'o str,
        target: ResolveTarget,
    ) -> Option<(u32, &'o str)> {
        if self.scope.file_meta(file).language != Language::CSharp
            || split_qualifier(name).is_some()
            || !matches!(target, ResolveTarget::Callable | ResolveTarget::Method)
        {
            return None;
        }
        let owner = owner_key(owner)?;
        let ty = self.first_in_file(file, owner, ResolveTarget::Type, None)?;
        Some((ty, owner))
    }

    /// The index's `Resolver::csharp_field_receiver_member` before its
    /// access rule: `member` of `field.member()` on the type of the field or
    /// property `field` that the caller's own type `owner` (an `owner_key`)
    /// declares in another `partial` part or inherits. `None` for any other
    /// site, or when that lookup binds nothing.
    fn csharp_field_receiver(
        &self,
        file: &str,
        owner: Option<&str>,
        site: CallSite<'_>,
        types: &OverlayNames<'_>,
    ) -> Option<(u32, f32)> {
        let CallSite::FromReceiver { receiver, member } = site.fallback() else {
            return None;
        };
        let owner = owner?;
        let ty = self.first_in_file(file, owner, ResolveTarget::Type, None)?;
        let field = type_ladder::csharp_member_field(self, ty, owner, receiver)?;
        self.chained_member_outcome((field, 1.0), member, Language::CSharp, types)
            .bound()
    }

    /// The index's `Resolver::partial_type_member`: the member a bare C#
    /// call `name()` reaches in its caller's own type `owner` when that type
    /// has `partial` parts ([`Self::partial_group`]); [`LadderMember::NoEdge`]
    /// for any other call.
    fn partial_type_member(
        &self,
        file: &str,
        name: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> LadderMember {
        let Some((ty, owner)) = self
            .partial_caller_type(file, name, owner, target)
            .filter(|&(ty, _)| !self.partial_group(ty).is_empty())
        else {
            return LadderMember::NoEdge;
        };
        type_ladder::member_via_type(
            self,
            TypeCandidates::Unique(ty, owner),
            file,
            name,
            target,
            true,
        )
    }

    /// Whether the partial-type tier may bind a bare C# call `name()` that
    /// the overlay cannot see ([`GAP_CS_PARTIAL_BARE_CALL`]): the overlay
    /// shows the caller's type `owner` no parts, yet a C# type of the same
    /// name, kind and scope in another file owns or inherits `name`. The
    /// index merges an attribute class as a class, so an `Annotation`
    /// compares as a `Class`.
    fn unseen_part_owns(&self, file: &str, name: &str, owner: &str) -> bool {
        let Some((ty, owner)) =
            self.partial_caller_type(file, name, owner, ResolveTarget::Callable)
        else {
            return false;
        };
        let merge_kind = |kind: NodeKind| match kind {
            NodeKind::Annotation => NodeKind::Class,
            kind => kind,
        };
        // A file-scoped namespace stamps no owner: its name tells two
        // scopes apart. A scope that stays unknown may match.
        let namespace = |id: u32| {
            self.node_file(id).and_then(|file| {
                type_ladder::declared_namespace(
                    self.owner_of(id),
                    self.csharp_namespaces(file).iter().copied(),
                )
            })
        };
        let kind = merge_kind(self.kind_of(ty));
        let scope = self.owner_of(ty);
        // Read on the first namesake only: it builds the namespace table.
        let my_namespace = OnceCell::new();
        // Cheapest test first: most types have no namesake.
        let namesakes: Vec<u32> = self
            .global_named(owner)
            .into_iter()
            .filter(|&other| {
                other != ty
                    && merge_kind(self.kind_of(other)) == kind
                    && self.owner_of(other) == scope
                    && self.node_file(other).is_some_and(|other_file| {
                        other_file != file && self.meta_of(other).language == Language::CSharp
                    })
                    && match (
                        *my_namespace.get_or_init(|| namespace(ty)),
                        namespace(other),
                    ) {
                        (Some(mine), Some(theirs)) => mine == theirs,
                        _ => true,
                    }
            })
            .collect();
        if namesakes.is_empty() || !self.partial_group(ty).is_empty() {
            return false;
        }
        namesakes.into_iter().any(|other| {
            owned_or_inherited(type_ladder::member_via_type(
                self,
                TypeCandidates::Unique(other, owner),
                file,
                name,
                ResolveTarget::Callable,
                true,
            ))
            .is_some()
        })
    }

    /// Whether the base `base` of the clean type `ty` names one of `traits`,
    /// the dirty files' archived trait impls for `ty`, and no clean file's
    /// `impl` block implements it for `ty`.
    fn only_dirty_impl_gives(&self, ty: u32, base: u32, traits: &[&str]) -> bool {
        let Some(base_key) = owner_key(self.name_of(base)) else {
            return false;
        };
        let pool = &self.graph.string_pool;
        traits.contains(&base_key)
            && !self.graph.nodes_by_name(self.name_of(ty)).any(|idx| {
                let block = &self.graph.nodes[idx as usize];
                NodeKind::from(&block.kind) == NodeKind::Impl
                    && !self.dirty_base.contains(&idx)
                    && owner_key(block.owner_class.resolve(pool)) == Some(base_key)
            })
    }

    /// The index's `SymbolTable::owner_path_agrees`: [`owner_path_agrees`]
    /// over the types `file` declares.
    fn owner_path_agrees(&self, file: &str, path: &str) -> bool {
        owner_path_agrees(
            path,
            |name| {
                let mut owners = self
                    .decls_in_file(file, name)
                    .into_iter()
                    .filter(|&(_, kind)| kind.is_type())
                    .map(|(id, _)| self.owner_of(id));
                match (owners.next(), owners.next()) {
                    (None, _) => DeclaredType::Undeclared,
                    (Some(_), Some(_)) => DeclaredType::Ambiguous,
                    (Some(owner), None) => DeclaredType::Unique(owner),
                }
            },
            owner_key,
        )
    }

    /// The index's `Resolver::type_candidates`, ladder step 1: the project
    /// types `qualifier` can name in `source`, by the caller's own file, a
    /// C# `using` alias, an import binding the name, then the global name
    /// table. A library import names [`TypeCandidates::External`].
    fn type_candidates<'q>(
        &self,
        source: &str,
        qualifier: &'q str,
        imports: &'q [RawImport],
    ) -> TypeCandidates<'q> {
        if let Some(id) = self.first_in_file(source, qualifier, ResolveTarget::Type, None) {
            return TypeCandidates::Unique(id, qualifier);
        }
        let caller = self.scope.file_meta(source);
        let language = caller.language;
        if language == Language::CSharp {
            if let Some(path) = imports.iter().find_map(|import| {
                (import.alias.as_deref() == Some(qualifier))
                    .then_some(import.imported_name.as_str())
            }) {
                return self.csharp_alias_type(source, path);
            }
        }
        let mut imported_in_project = false;
        for import in imports {
            if import
                .alias
                .as_deref()
                .unwrap_or(import.imported_name.as_str())
                != qualifier
            {
                continue;
            }
            if fqn_language(language) {
                let exported = fqn_imported_name(import);
                let in_file =
                    |file: &str| self.first_in_file(file, exported, ResolveTarget::Type, None);
                let hit = match self.scope.module(source, &import.source, language) {
                    Module::Unique(file)
                        if !self.owner_path_agrees(file, fqn_type_path(import)) =>
                    {
                        return TypeCandidates::None;
                    }
                    Module::Unique(file) => in_file(file),
                    Module::Namespace(files) => {
                        let mut hits = files.iter().filter_map(|&file| in_file(file));
                        hits.next().filter(|_| hits.next().is_none())
                    }
                    Module::Missing
                        if language != Language::Kotlin
                            || ladder::import_is_external(
                                &self.scope,
                                source,
                                import,
                                language,
                            ) =>
                    {
                        return TypeCandidates::External;
                    }
                    Module::Missing | Module::Ambiguous => None,
                };
                if let Some(id) = hit {
                    return TypeCandidates::Unique(id, exported);
                }
                imported_in_project = true;
                continue;
            }
            let exported = import.imported_name.as_str();
            let mut in_project = false;
            let mut hit = None;
            self.for_each_candidate(source, &import.source, &mut |candidate| {
                // The index's rule: a file of a language the import cannot
                // name is no candidate.
                if !self.has_file(candidate)
                    || !language_reaches(caller, self.scope.file_meta(candidate))
                {
                    return true;
                }
                in_project = true;
                hit = self.first_in_file(candidate, exported, ResolveTarget::Type, None);
                hit.is_none()
            });
            if let Some(id) = hit {
                return TypeCandidates::Unique(id, exported);
            }
            if !in_project {
                return TypeCandidates::External;
            }
            imported_in_project = true;
        }
        if caller.language == Language::Rust && !imported_in_project {
            return TypeCandidates::None;
        }
        // The parts of one C# partial type are one candidate type.
        let ids: Vec<u32> = type_ladder::distinct_types(
            self,
            TypeFacts::global_candidates(self, qualifier, ResolveTarget::Type, caller).into_iter(),
        )
        .collect();
        match ids.len() {
            0 => TypeCandidates::None,
            1 => TypeCandidates::Unique(ids[0], qualifier),
            _ => TypeCandidates::Set(ids, qualifier),
        }
    }

    /// The index's `Resolver::csharp_alias_type`: the project types a C#
    /// `using Alias = Ns.Type;` names, by last segment and whole path.
    fn csharp_alias_type<'q>(&self, source: &str, path: &'q str) -> TypeCandidates<'q> {
        let Some(name) = owner_key(path) else {
            return TypeCandidates::External;
        };
        let mut ids = TypeFacts::global_candidates(
            self,
            name,
            ResolveTarget::Type,
            self.scope.file_meta(source),
        );
        ids.retain(|&id| {
            self.node_file(id)
                .is_some_and(|file| self.owner_path_agrees(file, path))
        });
        let ids: Vec<u32> = type_ladder::distinct_types(self, ids.into_iter()).collect();
        match ids.len() {
            0 => TypeCandidates::External,
            1 => TypeCandidates::Unique(ids[0], name),
            _ => TypeCandidates::Set(ids, name),
        }
    }

    /// The index's receiver-typing ladder (`Resolver::resolve_member_via_type`)
    /// for the qualified `callee` = `qualifier` + `member` of `source`, which
    /// runs where the qualifier tiers found nothing.
    fn receiver_ladder(
        &self,
        source: &str,
        imports: &[RawImport],
        (callee, qualifier, member): (&str, &str, &str),
        target: ResolveTarget,
        call: bool,
    ) -> LadderMember {
        if !matches!(target, ResolveTarget::Callable | ResolveTarget::Method)
            || !qualifier_prefix_is_internal(callee, qualifier)
        {
            return LadderMember::NoEdge;
        }
        let candidates = self.type_candidates(source, qualifier, imports);
        type_ladder::member_via_type(self, candidates, source, member, target, call)
    }

    /// [`Self::imports`] of `file` as [`RawImport`]s, converted once.
    fn raw_imports(&self, file: &str) -> Rc<[RawImport]> {
        if let Some(imports) = self.raw_imports.borrow().get(file) {
            return Rc::clone(imports);
        }
        let imports: Rc<[RawImport]> = self
            .imports(file)
            .map(|imports| {
                imports
                    .iter()
                    .map(|import| RawImport {
                        source: import.source.to_owned(),
                        imported_name: import.imported_name.to_owned(),
                        alias: import.alias.map(str::to_owned),
                        binding_kind: import.binding_kind,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.raw_imports
            .borrow_mut()
            .insert(file.to_owned(), Rc::clone(&imports));
        imports
    }

    /// The index's `Resolver::resolve_chained_member_outcome`: the member `member`
    /// of the type a call to `callee` returns, read in `callee`'s own file
    /// and found by the receiver-typing ladder, at the ladder's confidence
    /// capped by `inner_confidence`. Import and namespace visibility is read
    /// in the callee's file, including clean files' archived import facts.
    fn chained_member_outcome(
        &self,
        (callee, inner_confidence): (u32, f32),
        member: &str,
        caller: Language,
        types: &OverlayNames<'_>,
    ) -> type_ladder::ChainOutcome<(u32, f32)> {
        let Some(ty) = type_ladder::returned_type(self, callee) else {
            return type_ladder::ChainOutcome::Unknown;
        };
        let Some(callee_file) = self.node_file(callee) else {
            return type_ladder::ChainOutcome::Unknown;
        };
        let language = self.meta_of(callee).language;
        let imports = self.raw_imports(callee_file);
        let candidates = if let Some(id) =
            self.first_in_file(callee_file, ty, ResolveTarget::Type, None)
        {
            TypeCandidates::Unique(id, ty)
        } else {
            match language {
                Language::Java | Language::Kotlin
                    if imports
                        .iter()
                        .any(|import| import_binding(import, ty, language).is_some()) =>
                {
                    let hit = ladder::imported_member(
                        &OverlayImports {
                            facts: self,
                            names: types,
                            types,
                        },
                        callee_file,
                        language,
                        ty,
                        &imports,
                        types.target,
                        true,
                    );
                    match hit {
                        ladder::MemberHit::Bound(hit) => {
                            TypeCandidates::Unique(hit.id, self.name_of(hit.id))
                        }
                        ladder::MemberHit::Unbound { .. } => {
                            return type_ladder::ChainOutcome::Conclusive
                        }
                    }
                }
                Language::CSharp
                    if imports
                        .iter()
                        .any(|import| import.alias.as_deref() == Some(ty)) =>
                {
                    let Some(import) = imports
                        .iter()
                        .find(|import| import.alias.as_deref() == Some(ty))
                    else {
                        return type_ladder::ChainOutcome::Conclusive;
                    };
                    let path = import.imported_name.as_str();
                    let Some(name) = owner_key(path) else {
                        return type_ladder::ChainOutcome::Conclusive;
                    };
                    type_ladder::visible_return_type(self, callee_file, name, |id| {
                        self.node_file(id).is_some_and(|file| {
                            self.scope
                                .namespace_type_path_matches(file, self.owner_of(id), path)
                        })
                    })
                }
                Language::CSharp => type_ladder::csharp_visible_return_type(
                    self,
                    callee_file,
                    ty,
                    self.csharp_namespaces(callee_file).iter().copied(),
                    |id| {
                        self.node_file(id).is_some_and(|file| {
                            self.scope.namespace_return_type_visible(
                                file,
                                self.owner_of(id),
                                callee_file,
                                &imports,
                            )
                        })
                    },
                    |id, namespace| self.type_namespace(id) == Some(namespace),
                ),
                Language::Swift => type_ladder::visible_return_type(self, callee_file, ty, |id| {
                    self.node_file(id).is_some_and(|file| {
                        type_ladder::swift_return_type_visible(callee_file, file, &imports)
                    })
                }),
                Language::Python | Language::TypeScript | Language::JavaScript => {
                    self.type_candidates(callee_file, ty, &imports)
                }
                Language::Rust => match reexport::rust_imported_type(
                    self,
                    callee_file,
                    ty,
                    imports.iter().map(ImportRef::from),
                ) {
                    reexport::RustImportedType::Unique(id) => {
                        let Some(name) = type_ladder::TypeFacts::name_of(self, id) else {
                            return type_ladder::ChainOutcome::Conclusive;
                        };
                        TypeCandidates::Unique(id, name)
                    }
                    reexport::RustImportedType::External
                    | reexport::RustImportedType::Conclusive => {
                        return type_ladder::ChainOutcome::Conclusive;
                    }
                    reexport::RustImportedType::Unmodeled => {
                        match type_ladder::unmodeled_return_type(self, callee_file, ty) {
                            type_ladder::ChainOutcome::Bound(candidates) => candidates,
                            type_ladder::ChainOutcome::Conclusive => {
                                return type_ladder::ChainOutcome::Conclusive;
                            }
                            type_ladder::ChainOutcome::Unknown => {
                                return type_ladder::ChainOutcome::Unknown;
                            }
                        }
                    }
                },
                Language::Php
                    if imports.iter().any(|import| {
                        import.alias.as_deref().unwrap_or(&import.imported_name) == ty
                    }) =>
                {
                    self.type_candidates(callee_file, ty, &imports)
                }
                Language::Cpp | Language::Dart if !imports.is_empty() => {
                    match reexport::imported_file_type(
                        self,
                        callee_file,
                        language,
                        ty,
                        imports.iter().map(ImportRef::from),
                    ) {
                        reexport::ImportedFileType::Unique(id) => TypeCandidates::Unique(id, ty),
                        // An indirect or precompiled-header include reaches a
                        // header the graph does not follow; a same-directory
                        // declaration stays evidence for C++.
                        reexport::ImportedFileType::Missing
                            if language == Language::Cpp
                                && self.declares_type_beside(callee_file, ty) =>
                        {
                            self.type_candidates(callee_file, ty, &[])
                        }
                        reexport::ImportedFileType::Missing
                        | reexport::ImportedFileType::Ambiguous => {
                            return type_ladder::ChainOutcome::Conclusive;
                        }
                    }
                }
                // A Dart file without imports is a `part of` file, whose types
                // come from its library and that library's other parts, or a
                // library that sees no other project library. Its file alone
                // cannot show which sibling or namesake is in scope.
                Language::Dart => return type_ladder::ChainOutcome::Unknown,
                _ if self.declares_type_beside(callee_file, ty) => {
                    self.type_candidates(callee_file, ty, &[])
                }
                _ if language == Language::Go
                    && reexport::go_dot_import_blocks_unmodeled_type(
                        imports.iter().map(ImportRef::from),
                    ) =>
                {
                    return type_ladder::ChainOutcome::Unknown;
                }
                _ => match type_ladder::unmodeled_return_type(self, callee_file, ty) {
                    type_ladder::ChainOutcome::Bound(candidates) => candidates,
                    type_ladder::ChainOutcome::Conclusive => {
                        return type_ladder::ChainOutcome::Conclusive;
                    }
                    type_ladder::ChainOutcome::Unknown => {
                        return type_ladder::ChainOutcome::Unknown;
                    }
                },
            }
        };
        // The index's `call_target`: Rust method syntax reaches only methods.
        let target = if caller == Language::Rust {
            ResolveTarget::Method
        } else {
            ResolveTarget::Callable
        };
        let Some((id, confidence)) = ladder_edge(type_ladder::member_via_type(
            self,
            candidates,
            callee_file,
            member,
            target,
            true,
        )) else {
            return type_ladder::ChainOutcome::Conclusive;
        };
        type_ladder::ChainOutcome::Bound((id, confidence.min(inner_confidence)))
    }

    /// The index's `Resolver::csharp_extension` for a dirty C# caller, the
    /// edge only: the overlay records no BlindSpot. It reads the extension
    /// declarations of the clean files from their archived `FunctionMeta`,
    /// and those of the dirty files from their fresh parse. When a dirty C#
    /// method named `member` has no flags in its fragment, no edge binds and
    /// the result is `Err(DirtyNamesake)`: the caller records a [`CallGap`],
    /// so the payload says the overlay fell back.
    fn csharp_extension_edge(
        &self,
        source: &str,
        receiver: ReceiverSite<'_>,
        member: &str,
        arguments: Option<CallArguments<'_>>,
        imports: &[RawImport],
        owner: Option<&str>,
    ) -> Result<Option<(u32, f32)>, DirtyNamesake> {
        let graph = self.graph;
        let pool = &graph.string_pool;
        let mut decls: Vec<csharp_extension::ExtensionDecl<'_>> = Vec::new();
        let namespace_of = |file: &'a str, class: &'a str| {
            self.first_in_file(file, class, ResolveTarget::Type, None)
                .and_then(|class_id| {
                    type_ladder::declared_namespace(
                        self.owner_of(class_id),
                        self.csharp_namespaces(file).iter().copied(),
                    )
                })
        };
        // A later overload sits in no name table, yet the index records it
        // as an extension of its own.
        let dirty = self
            .dirty_by_name
            .get(member)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .flat_map(|&first| std::iter::once(first).chain(self.later_overloads_of(first)));
        for id in dirty {
            if self.kind_of(id) != NodeKind::Method || self.meta_of(id).language != Language::CSharp
            {
                continue;
            }
            let Some(symbol) = self.dirty_symbol(id) else {
                return Err(DirtyNamesake);
            };
            let Some(flags) = symbol.flags else {
                return Err(DirtyNamesake);
            };
            if !csharp_extension::is_extension(flags, Language::CSharp) {
                continue;
            }
            let (Some(file), Some(class)) = (self.node_file(id), self.owner_of(id)) else {
                continue;
            };
            decls.push(csharp_extension::ExtensionDecl {
                id,
                this_type: symbol.params.get(1).map_or("", String::as_str),
                flags,
                // `params` alternates name and type; [0] is `this`.
                params: symbol
                    .params
                    .iter()
                    .skip(2)
                    .step_by(2)
                    .map(String::as_str)
                    .collect(),
                file,
                class,
                namespace: namespace_of(file, class),
            });
        }
        for id in graph.nodes_by_name(member) {
            if self.dirty_base.contains(&id) || self.name_of(id) != member {
                continue;
            }
            let Some(meta) = base_function_meta(graph, id) else {
                continue;
            };
            let flags = meta.flags.to_native();
            if !csharp_extension::is_extension(flags, self.meta_of(id).language) {
                continue;
            }
            let (Some(file), Some(class)) = (self.node_file(id), self.owner_of(id)) else {
                continue;
            };
            decls.push(csharp_extension::ExtensionDecl {
                id,
                this_type: meta.params.get(1).map_or("", |ty| ty.resolve(pool)),
                flags,
                // `params` alternates name and type; [0] is `this`.
                params: meta
                    .params
                    .iter()
                    .skip(2)
                    .step_by(2)
                    .map(|name| name.resolve(pool))
                    .collect(),
                file,
                class,
                namespace: namespace_of(file, class),
            });
        }
        if decls.is_empty() {
            return Ok(None);
        }
        let held: Rc<[RawImport]>;
        let (name, scope_file, scope_imports): (Option<&str>, &str, &[RawImport]) = match receiver {
            ReceiverSite::Named(name) => (Some(name), source, imports),
            ReceiverSite::Returned(callee) => {
                let (Some(name), Some(file)) = (
                    type_ladder::returned_type(self, callee),
                    self.node_file(callee),
                ) else {
                    return Ok(None);
                };
                held = self.raw_imports(file);
                (Some(name), file, &held[..])
            }
            ReceiverSite::Untyped => (None, source, imports),
        };
        let types: Vec<u32> = match name.map(|name| {
            (
                name,
                self.first_in_file(scope_file, name, ResolveTarget::Type, None),
            )
        }) {
            None => Vec::new(),
            Some((_, Some(id))) => vec![id],
            Some((name, None)) => TypeFacts::global_candidates(
                self,
                name,
                ResolveTarget::Type,
                self.scope.file_meta(scope_file),
            )
            .into_iter()
            .filter(|&id| {
                self.node_file(id).is_some_and(|file| {
                    self.scope.namespace_return_type_visible(
                        file,
                        self.owner_of(id),
                        scope_file,
                        scope_imports,
                    )
                })
            })
            .collect(),
        };
        let verdict = csharp_extension::resolve(
            self,
            &decls,
            csharp_extension::Receiver {
                name,
                types: &types,
            },
            member,
            arguments,
            &csharp_extension::CallerScope {
                file: source,
                namespaces: self.csharp_namespaces(source),
                imports,
                owner,
            },
        );
        Ok(match verdict {
            csharp_extension::Verdict::Bound { id, confidence } => Some((id, confidence)),
            _ => None,
        })
    }

    /// The `FunctionMeta::flags` the C# access rule reads for `id`
    /// ([`csharp_access`]): a clean base method's archived flags. A dirty
    /// file's input carries no flags, so a dirty method reads the flags of
    /// the base declaration it replaced, which the edit may have changed. A
    /// method with no such declaration (an overload, whose anchor repeats)
    /// reads as 0 (public, instance), and the rule admits a call to it. A
    /// dirty method's own parse carries its flags ([`OverlaySymbol::flags`]);
    /// one without them is not known ([`Self::csharp_flags_known`]), and the
    /// caller reports a verdict that rests on them
    /// ([`GAP_CS_ACCESS_DIRTY_TARGET`]).
    fn csharp_method_flags(&self, id: u32) -> u16 {
        if let Some(&flags) = self.dirty_flags.get(&id) {
            return flags;
        }
        let base = match id.checked_sub(self.base_len()) {
            Some(offset) => self.nodes[offset as usize].replaced_base,
            None => (!self.dirty_base.contains(&id)).then_some(id),
        };
        base.and_then(|base| base_function_meta(self.graph, base))
            .map_or(0, |meta| meta.flags.to_native())
    }

    /// Whether [`Self::csharp_method_flags`] reads `id`'s own flags: a clean
    /// base method's, or a dirty method's from its fresh parse.
    fn csharp_flags_known(&self, id: u32) -> bool {
        id < self.base_len() || self.dirty_flags.contains_key(&id)
    }

    /// The fresh parse of the dirty node `id`; `None` for a base node.
    /// Found by the id's range, not by its path: two inputs of one path
    /// each number their own symbols.
    fn dirty_symbol(&self, id: u32) -> Option<&'a OverlaySymbol> {
        let file_ord = self
            .dirty_starts
            .partition_point(|&start| start <= id)
            .checked_sub(1)?;
        self.scope.dirty()[file_ord]
            .symbols
            .get((id - self.dirty_starts[file_ord]) as usize)
    }

    /// The index's `SymbolTable::c_callable`: the flags and the parameter
    /// list of a C or C++ function or method, from the archived
    /// `FunctionMeta` of a base node and from the fresh parse of a dirty
    /// one. `None` when no meta was paired with it.
    fn c_callable(&self, id: u32) -> Option<CCallable> {
        // The kind test first: it is cheap, and most candidates fail it.
        if !matches!(self.kind_of(id), NodeKind::Function | NodeKind::Method)
            || !is_c_family(self.meta_of(id).language)
        {
            return None;
        }
        match id.checked_sub(self.base_len()) {
            Some(_) => {
                let symbol = self.dirty_symbol(id)?;
                symbol
                    .flags
                    .map(|flags| CCallable::new(flags, symbol.params.iter().map(String::as_str)))
            }
            None => base_function_meta(self.graph, id).map(|meta| {
                let pool = &self.graph.string_pool;
                CCallable::new(
                    meta.flags.to_native(),
                    meta.params.iter().map(|text| text.resolve(pool)),
                )
            }),
        }
    }

    /// The namespaces the C# file `file` declares (`.`-joined), the index's
    /// `SymbolTable::csharp_namespaces`: a dirty file's from its fresh parse,
    /// a clean one's from its archived Namespace nodes.
    fn csharp_namespaces(&self, file: &str) -> &[&'a str] {
        self.csharp_namespaces
            .get_or_init(|| {
                let graph = self.graph;
                let pool = &graph.string_pool;
                let mut by_file: FxHashMap<&'a str, Vec<&'a str>> = FxHashMap::default();
                let mut note = |file: &'a str, namespace: &'a str| {
                    let declared = by_file.entry(file).or_default();
                    if !declared.contains(&namespace) {
                        declared.push(namespace);
                    }
                };
                for (idx, node) in graph.nodes.iter().enumerate() {
                    if NodeKind::from(&node.kind) != NodeKind::Namespace
                        || self.dirty_base.contains(&(idx as u32))
                    {
                        continue;
                    }
                    let Some(file) = graph.files.get(node.file_idx.to_native() as usize) else {
                        continue;
                    };
                    let path = file.path.resolve(pool);
                    if stored_language(graph, node.file_idx.to_native(), path) == Language::CSharp {
                        note(path, node.name.resolve(pool));
                    }
                }
                for file in self.scope.dirty() {
                    if file.file_meta().language != Language::CSharp {
                        continue;
                    }
                    for symbol in &file.symbols {
                        if symbol.kind == NodeKind::Namespace {
                            note(file.rel_path.as_str(), symbol.name.as_str());
                        }
                    }
                }
                by_file
            })
            .get(file)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The type the clean C# field or property `idx` declares, the index's
    /// `SymbolTable::return_type_of` (its `RawNode.type_annotation`).
    /// `graph.bin` keeps no text of it, only the `type_annotation` `Accesses`
    /// edge Pass 2 resolved from it: the name of that edge's one target.
    /// `None` for another language, whose index records no such fact.
    fn base_property_type(&self, idx: u32) -> Option<&'a str> {
        if self.meta_of(idx).language != Language::CSharp {
            return None;
        }
        let graph = self.graph;
        let pool = &graph.string_pool;
        let out = graph.out_offsets[idx as usize].to_native() as usize
            ..graph.out_offsets[idx as usize + 1].to_native() as usize;
        let mut types = graph.edges[out]
            .iter()
            .filter(|edge| {
                RelType::from(&edge.rel_type) == RelType::Accesses
                    && edge.reason.resolve(pool) == TYPE_ANNOTATION_REASON
            })
            .map(|edge| edge.target.to_native());
        match (types.next(), types.next()) {
            (Some(ty), None) => Some(graph.nodes[ty as usize].name.resolve(pool)),
            _ => None,
        }
    }

    /// The index's `SymbolTable::type_namespace`: the namespace the C# type
    /// `id` is declared in.
    fn type_namespace(&self, id: u32) -> Option<&'a str> {
        type_ladder::declared_namespace(
            self.owner_of(id),
            self.csharp_namespaces(self.node_file(id)?).iter().copied(),
        )
    }

    /// The index's `SymbolTable::declares_type_beside`: `file` or a file in
    /// its directory declares a type named `name`.
    fn declares_type_beside(&self, file: &str, name: &str) -> bool {
        let dir = std::path::Path::new(file).parent();
        let wanted = ResolveTarget::Type.kind_predicate();
        self.global_named(name).into_iter().any(|id| {
            wanted(self.kind_of(id))
                && self
                    .node_file(id)
                    .map(|other| std::path::Path::new(other).parent())
                    == Some(dir)
        })
    }

    /// Whether `callee` names a constructible type and a `Method` the
    /// `caller` file admits both ([`ladder::CallFacts::names_type_and_member`]).
    fn names_type_and_member(&self, callee: &str, caller: FileMeta) -> bool {
        let named = self.global_named(callee);
        named.iter().any(|&id| self.kind_of(id).is_constructible())
            && named
                .iter()
                .any(|&id| self.kind_of(id) == NodeKind::Method && caller.admits(self.meta_of(id)))
    }
}

/// The edge a receiver-typing ladder verdict emits, at the index's
/// confidence for its tier.
fn ladder_edge(verdict: LadderMember) -> Option<(u32, f32)> {
    Tier::of_member(verdict).map(|(id, tier)| (id, tier.confidence()))
}

/// [`ladder_edge`] of a member the one candidate type owns or inherits; no
/// edge for any other verdict.
fn owned_or_inherited(verdict: LadderMember) -> Option<(u32, f32)> {
    match verdict {
        LadderMember::Owned(_) | LadderMember::Inherited(_) => ladder_edge(verdict),
        _ => None,
    }
}

impl PickFacts for OverlayFacts<'_> {
    fn file_meta(&self, file: &str) -> FileMeta {
        self.scope.file_meta(file)
    }

    fn pick_candidates(&self, file: &str, name: &str) -> Cow<'_, [u32]> {
        let mut ids = Vec::new();
        for (id, _) in self.decls_in_file(file, name) {
            ids.push(id);
            if PickFacts::impl_member(self, id).is_some() {
                ids.extend(
                    self.later_overloads_of(id)
                        .into_iter()
                        .filter(|&later| PickFacts::impl_member(self, later).is_some()),
                );
            }
        }
        Cow::Owned(ids)
    }

    fn kind(&self, id: u32) -> NodeKind {
        self.kind_of(id)
    }

    fn owner(&self, id: u32) -> Option<&str> {
        self.owner_of(id)
    }

    fn impl_member(&self, id: u32) -> Option<ImplMember<'_>> {
        if id >= self.base_len() {
            self.dirty_impl_members.get(&id).copied()
        } else {
            self.base_impl_member(id)
        }
    }

    fn is_namespace_owner(&self, file: &str, owner: &str) -> bool {
        if self.scope.file_meta(file).language == Language::Go {
            return false;
        }
        if let Some(&file_ord) = self.dirty.get(file) {
            return self.dirty_namespaces[file_ord].contains(&owner);
        }
        let graph = self.graph;
        let Some(file_idx) = graph.file_idx_of(file) else {
            return false;
        };
        let namespaces = self.base_namespaces.get_or_init(|| {
            let mut namespaces: FxHashMap<u32, Vec<&str>> = FxHashMap::default();
            for kind in [NodeKind::Namespace, NodeKind::Module] {
                for idx in graph.nodes_by_kind(kind) {
                    if self.dirty_base.contains(&idx) {
                        continue;
                    }
                    let node = &graph.nodes[idx as usize];
                    if let Some(key) = owner_key(node.name.resolve(&graph.string_pool)) {
                        let keys = namespaces.entry(node.file_idx.to_native()).or_default();
                        if !keys.contains(&key) {
                            keys.push(key);
                        }
                    }
                }
            }
            namespaces
        });
        namespaces
            .get(&file_idx)
            .is_some_and(|keys| keys.contains(&owner))
            && !graph.nodes_by_name(owner).any(|idx| {
                let node = &graph.nodes[idx as usize];
                let kind = NodeKind::from(&node.kind);
                node.file_idx.to_native() == file_idx && (kind.is_type() || kind == NodeKind::Impl)
            })
    }

    fn namespace_visible_from(&self, reader: &str, target: &str) -> bool {
        self.scope
            .namespace_visible_from(reader, target, Language::Php)
    }

    fn file_of(&self, id: u32) -> Option<&str> {
        self.node_file(id)
    }

    fn is_lexically_scoped(&self, id: u32) -> bool {
        OverlayFacts::is_lexically_scoped(self, id)
    }

    fn is_body_local(&self, id: u32) -> bool {
        self.rust.is_body_local(id)
    }

    fn later_overloads(&self, first: u32) -> Cow<'_, [u32]> {
        Cow::Owned(self.later_overloads_of(first))
    }

    /// A clean base node reads the stored column, a dirty symbol the column
    /// its file's fresh parse gave in [`OverlayFacts::new`].
    fn container(&self, id: u32) -> Option<u32> {
        match id.checked_sub(self.base_len()) {
            Some(offset) => self
                .dirty_containers
                .get(offset as usize)
                .copied()
                .filter(|&c| c != NO_CONTAINER),
            None => self.graph.container(id),
        }
    }

    fn types_named(&self, owner: &str) -> Vec<u32> {
        let mut types = self.global_named(owner);
        types.retain(|&id| self.kind_of(id).is_type());
        types
    }

    fn ancestors(&self, ty: u32) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        let mut frontier = vec![ty];
        for _ in 0..MAX_HERITAGE_DEPTH {
            let level_start = out.len();
            for current in frontier {
                let Some((bases, _)) = TypeFacts::supertypes(self, current) else {
                    continue;
                };
                for &base in bases.iter() {
                    if base != ty && !out.contains(&base) {
                        out.push(base);
                    }
                }
            }
            if out.len() == level_start {
                break;
            }
            frontier = out[level_start..].to_vec();
        }
        out
    }
}

impl TypeFacts for OverlayFacts<'_> {
    fn supertypes(&self, ty: u32) -> Option<(Cow<'_, [u32]>, Option<u32>)> {
        if let Some((bases, first_unresolved)) =
            self.dirty_supertypes.get().and_then(|types| types.get(&ty))
        {
            return Some((Cow::Borrowed(bases.as_slice()), *first_unresolved));
        }
        if ty >= self.base_len() {
            return None;
        }
        let (bases, first_unresolved) = self.base_supertypes(ty)?;
        Some((Cow::Owned(bases), first_unresolved))
    }

    fn name_of(&self, id: u32) -> Option<&str> {
        Some(OverlayFacts::name_of(self, id))
    }

    fn owned_in_file(
        &self,
        file: &str,
        member: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> Vec<u32> {
        let Some(owner) = owner_key(owner) else {
            return Vec::new();
        };
        let wanted = target.kind_predicate();
        self.decls_in_file(file, member)
            .into_iter()
            .filter(|&(id, kind)| wanted(kind) && self.owner_of(id) == Some(owner))
            .map(|(id, _)| id)
            .collect()
    }

    fn owned_global(
        &self,
        member: &str,
        owner: &str,
        target: ResolveTarget,
        scope_file: &str,
    ) -> Vec<u32> {
        let Some(owner) = owner_key(owner) else {
            return Vec::new();
        };
        let scope_meta = self.scope.file_meta(scope_file);
        let scope_root = crate_root_prefix(scope_file);
        let same_package_only = scope_meta.language == Language::Go;
        let scope_dir = std::path::Path::new(scope_file).parent();
        let wanted = target.kind_predicate();
        let mut ids = self.global_named(member);
        ids.retain(|&id| {
            self.owner_of(id) == Some(owner)
                && wanted(self.kind_of(id))
                && scope_meta.admits(self.meta_of(id))
                && self.node_file(id).is_some_and(|f| {
                    crate_root_prefix(f) == scope_root
                        && (!same_package_only || std::path::Path::new(f).parent() == scope_dir)
                })
        });
        ids
    }

    fn count_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> usize {
        let wanted = target.kind_predicate();
        self.decls_in_file(file, name)
            .into_iter()
            .filter(|&(_, kind)| wanted(kind))
            .count()
    }

    fn global_candidates(&self, name: &str, target: ResolveTarget, caller: FileMeta) -> Vec<u32> {
        let wanted = target.kind_predicate();
        let mut ids = self.global_named(name);
        ids.retain(|&id| {
            wanted(self.kind_of(id))
                && caller.admits(self.meta_of(id))
                && !self.is_lexically_scoped(id)
        });
        ids
    }

    fn is_copy(&self, a: u32, b: u32) -> bool {
        OverlayFacts::is_copy(self, a, b)
    }

    fn nearest_copy(&self, caller_file: &str, candidates: &[u32]) -> Option<u32> {
        self.nearest_of_copies(caller_file, candidates)
    }

    fn partial_parts(&self, ty: u32) -> Cow<'_, [u32]> {
        Cow::Owned(self.partial_group(ty))
    }

    fn partial_type(&self, ty: u32) -> u32 {
        self.partial_group(ty).first().copied().unwrap_or(ty)
    }

    fn hidden_member(&self, id: u32) -> bool {
        let hidden = |id: u32| {
            csharp_access::visibility(self.csharp_method_flags(id)) == csharp_access::EXPLICIT_IMPL
        };
        hidden(id) && self.later_overloads_of(id).into_iter().all(hidden)
    }

    /// A dirty function's from its fresh parse; a clean one's from the
    /// archived `FunctionMeta`, which the index wrote from the same
    /// `RawFunctionMeta.return_type`; a clean C# field or property's from
    /// its type's `Accesses` edge ([`Self::base_property_type`]). A base node
    /// of a dirty file is stale. A dirty C# field or property's from its
    /// fresh `type_annotation`, as the index notes it; one of another
    /// language has no type fact, as in the index.
    fn return_type_of(&self, callee: u32) -> Option<&str> {
        if callee >= self.base_len() {
            return self.dirty_return_types.get(&callee).copied();
        }
        if self.dirty_base.contains(&callee) {
            return None;
        }
        if self.kind_of(callee) == NodeKind::Property {
            return self.base_property_type(callee);
        }
        let graph = self.graph;
        let metas = &graph.function_metas;
        let at = metas
            .binary_search_by_key(&callee, |meta| meta.node_idx.to_native())
            .ok()?;
        let return_type = metas[at].return_type.resolve(&graph.string_pool);
        (!return_type.is_empty()).then_some(return_type)
    }
}

impl ResolveFacts for OverlayFacts<'_> {
    fn call_decls(&self, file: &str, name: &str) -> impl Iterator<Item = (u32, NodeKind)> + Clone {
        ModuleFacts::decls_in_file(self, file, name).into_iter()
    }

    fn first_decl_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> Option<u32> {
        self.first_in_file(file, name, target, None)
    }

    /// `OverlayFacts::global_named` is not empty, read without building
    /// the list.
    fn has_named(&self, name: &str) -> bool {
        let graph = self.graph;
        self.dirty_by_name
            .get(name)
            .is_some_and(|ids| !ids.is_empty())
            || graph.nodes_by_name(name).any(|idx| {
                !self.dirty_base.contains(&idx)
                    && !graph.is_later_overload(idx)
                    && fragment_emits(NodeKind::from(&graph.nodes[idx as usize].kind))
            })
    }

    fn enum_variant_of(&self, type_id: u32, type_name: &str, variant: &str) -> Option<u32> {
        OverlayFacts::enum_variant_of(self, type_id, type_name, variant)
    }

    /// A dirty file's fresh `Impl` symbols; a clean file's archived `Impl`
    /// nodes ([`Self::base_impls`]).
    fn any_trait_impl(
        &self,
        file: &str,
        trait_key: &str,
        mut accepts: impl FnMut(&str) -> bool,
    ) -> bool {
        if let Some(&file_ord) = self.dirty.get(file) {
            return self.scope.dirty()[file_ord].symbols.iter().any(|s| {
                s.kind == NodeKind::Impl
                    && s.owner_class.as_deref().and_then(owner_key) == Some(trait_key)
                    && accepts(s.name.as_str())
            });
        }
        let Some(file_idx) = self.clean_file_idx(file) else {
            return false;
        };
        let graph = self.graph;
        self.base_impls().get(&file_idx).is_some_and(|impls| {
            impls.iter().any(|&(_, idx, implemented)| {
                implemented == Some(trait_key)
                    && accepts(graph.nodes[idx as usize].name.resolve(&graph.string_pool))
            })
        })
    }
}

/// The owner keys of the namespaces and modules among `symbols`, one dirty
/// file, that name no type the file declares: the index's
/// `SymbolTable::build_owner_kinds` for that file.
fn namespace_owners(symbols: &[OverlaySymbol]) -> Vec<&str> {
    let mut owners: Vec<&str> = Vec::new();
    for symbol in symbols {
        if !matches!(symbol.kind, NodeKind::Namespace | NodeKind::Module) {
            continue;
        }
        let Some(key) = owner_key(&symbol.name) else {
            continue;
        };
        let declares_type = symbols.iter().any(|other| {
            other.name == key && (other.kind.is_type() || other.kind == NodeKind::Impl)
        });
        if !declares_type && !owners.contains(&key) {
            owners.push(key);
        }
    }
    owners
}

impl ImplNode for OverlaySymbol {
    fn name(&self) -> &str {
        &self.name
    }

    fn owner_class(&self) -> Option<&str> {
        self.owner_class.as_deref()
    }
}

/// The two lookups the index's `resolve_name_ref` and `resolve_member_read`
/// in `builder.rs` hand the shared policy ([`ladder::name_ref`],
/// [`resolve_member_read`]), passed to `run`: the name tiers
/// ([`name_tiers()`]) over the name table of the target's kinds, and the
/// import-member tier alone, which reads the fields.
#[allow(clippy::too_many_arguments)]
fn with_overlay_name_lookups<R>(
    graph: &ArchivedZeroCopyGraph,
    file_ord: usize,
    file: &OverlayFileInput,
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    callables: &OverlayNames<'_>,
    values: &OverlayNames<'_>,
    fields: &OverlayNames<'_>,
    types: &OverlayNames<'_>,
    qualifiers: &OverlayNames<'_>,
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    run: impl FnOnce(&mut NameLookup<'_>, &mut NameLookup<'_>) -> R,
) -> R {
    let base_metas = RefCell::new(base_metas);
    run(
        &mut |name: &str, target: ResolveTarget| {
            let names = match target {
                ResolveTarget::Callable => callables,
                ResolveTarget::Value => values,
                // A Python value read that may name a type alias.
                ResolveTarget::Type => types,
                _ => return Vec::new(),
            };
            let tiers = OverlayTiers::new(
                graph,
                file_ord,
                caller,
                &base_metas,
                (names, types, qualifiers),
                (nodes, replaced, dirty_base),
                facts,
            );
            let policy = TierPolicy::lookup(caller.language, target, &[]);
            name_tiers(&tiers, name, &file.imports, policy)
                .map(TierHit::edge)
                .into_iter()
                .collect()
        },
        // The name tiers ask the import tier alone for a field only. The
        // explicit imports only: the index's `resolve_imported_symbol` takes
        // the shared import tiers ([`import_tiers`]), wildcard pass included
        // (divergence, `ladder_import_member_parity`).
        &mut |name: &str, _: ResolveTarget| match ladder::imported_member(
            &OverlayImports {
                facts,
                names: fields,
                types,
            },
            facts.scope.dirty()[file_ord].rel_path.as_str(),
            caller.language,
            name,
            &file.imports,
            fields.target,
            false,
        ) {
            ladder::MemberHit::Bound(hit) => vec![(hit.id, Tier::ImportScoped.confidence())],
            ladder::MemberHit::Unbound { .. } => Vec::new(),
        },
    )
}

/// One name lookup [`with_overlay_name_lookups`] hands its `run`.
type NameLookup<'l> = dyn FnMut(&str, ResolveTarget) -> Vec<(u32, f32)> + 'l;

/// The decorators of the merged-space `id`, as the index reads them for a
/// member read's target: a dirty node's from its fresh parse
/// ([`OverlaySymbol::decorators`]), a clean base node's from its
/// `FunctionMeta`. A base node of a dirty file is stale and has none; the
/// name tiers never return one.
fn overlay_decorators<'a>(facts: &OverlayFacts<'a>, id: u32) -> Vec<&'a str> {
    if id < facts.base_len() {
        if facts.dirty_base.contains(&id) {
            return Vec::new();
        }
        return base_function_meta(facts.graph, id).map_or_else(Vec::new, |meta| {
            meta.decorators
                .iter()
                .map(|decorator| decorator.resolve(&facts.graph.string_pool))
                .collect()
        });
    }
    facts.dirty_symbol(id).map_or_else(Vec::new, |symbol| {
        symbol.decorators.iter().map(String::as_str).collect()
    })
}

/// The kind of the merged-space `idx`.
fn node_kind(graph: &ArchivedZeroCopyGraph, nodes: &[ViewNode], idx: u32) -> NodeKind {
    match idx.checked_sub(graph.nodes.len() as u32) {
        Some(offset) => nodes[offset as usize].kind,
        None => NodeKind::from(&graph.nodes[idx as usize].kind),
    }
}

/// The clean base declarations of `target`'s kinds named `name`, via the
/// name index. Dirty-file base nodes are excluded: replaced ones already
/// take part as overlay declarations of the same name, suppressed ones no
/// longer exist. A later overload is in no name table of the index.
fn clean_base(
    graph: &ArchivedZeroCopyGraph,
    target: ResolveTarget,
    dirty_base: &FxHashSet<u32>,
    name: &str,
) -> Vec<u32> {
    graph
        .nodes_by_name(name)
        .filter(|&idx| {
            target.kind_predicate()(NodeKind::from(&graph.nodes[idx as usize].kind))
                && !dirty_base.contains(&idx)
                && !graph.is_later_overload(idx)
        })
        .collect()
}

/// The path of the file the base node `idx` is declared in.
fn base_path(graph: &ArchivedZeroCopyGraph, idx: u32) -> Option<&str> {
    graph
        .files
        .get(graph.nodes[idx as usize].file_idx.to_native() as usize)
        .map(|f| f.path.resolve(&graph.string_pool))
}

/// The overlay's facts for one name lookup of the dirty file `file_ord`
/// ([`name_tiers()`], the index's `Resolver::resolve_symbol_with_import_binding`
/// over the merged space): the view's facts, the name table of the
/// lookup's kinds, the type and qualifier tables, and the base file metas
/// Tier 3 reads. A dirty file was fully re-parsed, so its own declarations
/// are authoritative; a base candidate excludes every dirty-base node, so a
/// hit never needs a replaced-base redirect and never lands on a suppressed
/// node. The memos of the qualifier tier, the named-import tier and Tier 3
/// live in [`OverlayFacts`], per file.
struct OverlayTiers<'v, 'a, 'm> {
    graph: &'v ArchivedZeroCopyGraph,
    file_ord: usize,
    caller: FileMeta,
    /// The language the lookup resolves in: the caller's, or for an
    /// `Imports` edge the importer's as the index reads it.
    language: Language,
    /// The import-member tier's facts over the name table of the lookup's
    /// kinds.
    members: OverlayImports<'v, 'a, 'v>,
    qualifiers: OverlayQualifiers<'v, 'a, 'v>,
    nodes: &'v [ViewNode],
    replaced: &'v FxHashMap<u32, u32>,
    dirty_base: &'v FxHashSet<u32>,
    base_metas: &'v RefCell<&'m mut FxHashMap<usize, FileMeta>>,
}

impl<'v, 'a, 'm> OverlayTiers<'v, 'a, 'm> {
    /// `names` is the table of the lookup's kinds, a superset of them for a
    /// Rust method-syntax call (`ResolveTarget::Method` over the callables).
    fn new(
        graph: &'v ArchivedZeroCopyGraph,
        file_ord: usize,
        caller: FileMeta,
        base_metas: &'v RefCell<&'m mut FxHashMap<usize, FileMeta>>,
        (names, types, qualifiers): (
            &'v OverlayNames<'v>,
            &'v OverlayNames<'v>,
            &'v OverlayNames<'v>,
        ),
        (nodes, replaced, dirty_base): (
            &'v [ViewNode],
            &'v FxHashMap<u32, u32>,
            &'v FxHashSet<u32>,
        ),
        facts: &'v OverlayFacts<'a>,
    ) -> Self {
        Self {
            graph,
            file_ord,
            caller,
            language: caller.language,
            members: OverlayImports {
                facts,
                names,
                types,
            },
            qualifiers: OverlayQualifiers {
                facts,
                file_ord,
                qualifiers,
            },
            nodes,
            replaced,
            dirty_base,
            base_metas,
        }
    }

    /// The index's `rust_tree_import` ([`name_tiers::rust_tree_import`]) over
    /// the overlay's module trees. The `Imports` pass reads no tree
    /// ([`OverlayFacts::tree_free`]), as the index's import resolver has
    /// none.
    fn rust_tree_import(
        &self,
        name: &str,
        imports: &[RawImport],
        target: ResolveTarget,
    ) -> Option<reexport::NamedImport> {
        let facts = self.members.facts;
        let importer = self.source();
        if facts.tree_free.get() || !is_rust_source(std::path::Path::new(importer)) {
            return None;
        }
        facts.with_rust_tree(self.file_ord, |tree| {
            name_tiers::rust_tree_import(tree, &self.members, importer, name, imports, target)
        })
    }
}

impl<'v, 'a> NameTierFacts for OverlayTiers<'v, 'a, '_> {
    type Decls = OverlayFacts<'a>;
    type Imports = OverlayImports<'v, 'a, 'v>;
    type Qualifiers = OverlayQualifiers<'v, 'a, 'v>;
    type Modules = OverlayFacts<'a>;
    const STEM: StemFile = StemFile::DeclaringMember;

    fn decls(&self) -> &OverlayFacts<'a> {
        self.members.facts
    }

    fn import_members(&self) -> &OverlayImports<'v, 'a, 'v> {
        &self.members
    }

    fn qualifiers(&self) -> &OverlayQualifiers<'v, 'a, 'v> {
        &self.qualifiers
    }

    fn modules(&self) -> &OverlayFacts<'a> {
        self.members.facts
    }

    fn source(&self) -> &str {
        self.members.facts.scope.dirty()[self.file_ord]
            .rel_path
            .as_str()
    }

    fn caller(&self) -> FileMeta {
        self.caller
    }

    fn language(&self) -> Language {
        self.language
    }

    fn partial_type_member(&self, name: &str, owner: &str, target: ResolveTarget) -> LadderMember {
        self.members
            .facts
            .partial_type_member(self.source(), name, owner, target)
    }

    /// Memoized per file and kinds: the import list is always the file's
    /// own, and the importer's language fixes whether a re-export is
    /// followed. The index's `may_reach` gate only skips a name that no
    /// declaration and no alias carries, which no re-export can bind: a
    /// pre-filter, not a rule, so following every miss gives the same
    /// answer.
    fn named_import(
        &self,
        name: &str,
        imports: &[RawImport],
        target: ResolveTarget,
    ) -> Option<reexport::NamedImport> {
        let facts = self.members.facts;
        let key = (self.file_ord, target);
        if let Some(hit) = facts
            .named_imports
            .borrow()
            .get(&key)
            .and_then(|hits| hits.get(name))
        {
            return hit.clone();
        }
        let follow = is_ecmascript(self.language);
        let hit = reexport::named_import(
            facts,
            self.source(),
            imports.iter().map(ImportRef::from),
            name,
            target,
            |_| follow,
        )
        .or_else(|| self.rust_tree_import(name, imports, target));
        debug_assert!(hit
            .as_ref()
            .is_none_or(|hit| !self.replaced.contains_key(&hit.decl)));
        facts
            .named_imports
            .borrow_mut()
            .entry(key)
            .or_default()
            .insert(name.into(), hit.clone());
        hit
    }

    /// The overlay's module trees ([`RustModules`]); none in the `Imports`
    /// pass ([`OverlayFacts::tree_free`]).
    fn module_tree_member(&self, site: &ladder::QualifiedSite<'_>) -> Option<u32> {
        let facts = self.members.facts;
        if facts.tree_free.get() {
            return None;
        }
        let importer = self.source();
        facts.with_rust_tree(self.file_ord, |tree| {
            ladder::module_tree_member(
                tree,
                facts,
                importer,
                site.imports,
                site.callee,
                site.member,
                site.target,
            )
        })
    }

    fn qualified<'h>(
        &self,
        site: ladder::QualifiedSite<'_>,
        heritage: &'h [String],
    ) -> (Resolution, Option<&'h str>) {
        let hit = self.members.facts.qualified(
            self.file_ord,
            site,
            self.qualifiers.qualifiers,
            heritage,
            || self.module_tree_member(&site),
        );
        (hit, None)
    }

    fn receiver_ladder(
        &self,
        imports: &[RawImport],
        names: (&str, &str, &str),
        target: ResolveTarget,
        call: Option<CallPick<'_>>,
    ) -> LadderMember {
        self.members
            .facts
            .receiver_ladder(self.source(), imports, names, target, call.is_some())
    }

    /// The clean base and the overlay declarations named `name` in the kinds
    /// of the lookup, memoized per caller file. Same invariant as Tier 2: a
    /// base candidate is clean by construction, never redirected; overlay
    /// candidates are virtual indices.
    fn global(
        &self,
        name: &str,
        imports: &[RawImport],
        target: ResolveTarget,
        call: Option<CallPick<'_>>,
        heritage: &[String],
        form: SiteForm,
    ) -> Resolution {
        let facts = self.members.facts;
        let (nearest, free_only) = (call.is_some(), call.is_some_and(|call| call.free_only));
        let owner = call.and_then(|call| call.owner);
        // The key holds all this lookup reads of `call` and the site:
        // `global_tier` reads only `call.is_some()`, this method `free_only`,
        // and the admission gate the form, and the owner and the heritage
        // where a rule of that form reads them. A `CallPick` field or a gate
        // input any of them starts to read must join the key, or two call
        // sites that differ in it share one memoized answer.
        let language = self.caller.language;
        let key = GlobalKey {
            file_ord: self.file_ord,
            imports: (imports.as_ptr() as usize, imports.len()),
            name: Box::from(name),
            target,
            nearest,
            free_only,
            owner: owner.filter(|_| form.reads_owner(language)).map(Box::from),
            heritage: if form.reads_heritage(language) {
                (heritage.as_ptr() as usize, heritage.len())
            } else {
                (0, 0)
            },
            form,
        };
        if let Some(&hit) = facts.global.borrow().get(&key) {
            return hit;
        }
        let (graph, nodes) = (self.graph, self.nodes);
        // The ladder reads the candidates lazily, so the meta table sits in a
        // cell: a `Fn` stream fills it as the pick advances.
        let base_meta = |idx: u32| {
            let file_idx = graph.nodes[idx as usize].file_idx.to_native() as usize;
            *self
                .base_metas
                .borrow_mut()
                .entry(file_idx)
                .or_insert_with(|| stored_file_meta(graph, file_idx as u32))
        };
        // I5 (a scoped node is no global candidate) is the ladder's filter. The
        // field is the index's (`SymbolTable::global_field_candidates`).
        let field = ladder::global_field(target, free_only);
        let admits = |&(idx, _): &(u32, FileMeta)| field(node_kind(graph, nodes, idx));
        let overlay_candidates: &[(u32, FileMeta)] = self
            .members
            .names
            .anywhere
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let clean_candidates = clean_base(graph, target, self.dirty_base, name);
        let gate = Gate::new(
            facts,
            Site {
                caller: self.caller,
                caller_file: self.source(),
                caller_owner: owner,
                heritage,
                form,
                name,
                imports,
            },
        );
        // Read up to three times by the ladder (the pick, the nearest copy, the
        // count), and only on a memo miss. The pick stops at the second
        // admitted candidate, so a common name meets few of its base metas.
        let (clean, base_meta, admits, gate) =
            (clean_candidates.as_slice(), &base_meta, &admits, &gate);
        let candidates = move || {
            gate.admitted(
                clean
                    .iter()
                    .map(move |&idx| (idx, base_meta(idx)))
                    .chain(overlay_candidates.iter().copied())
                    .filter(admits),
            )
        };
        let hit = gate.settle(
            match c_family_field(facts, self.caller, self.source(), candidates, |id| {
                facts.c_callable(id)
            }) {
                Some(field) => name_tiers::global_tier(self, name, imports, target, call, || {
                    field.iter().copied()
                }),
                None => name_tiers::global_tier(self, name, imports, target, call, candidates),
            },
        );
        debug_assert!(hit
            .edge()
            .is_none_or(|(target, _)| !self.replaced.contains_key(&target)));
        facts.global.borrow_mut().insert(key, hit);
        hit
    }

    fn decl_file(&self, id: u32) -> Option<&str> {
        match id.checked_sub(self.graph.nodes.len() as u32) {
            Some(offset) => Some(&*self.nodes[offset as usize].rel_path),
            None => base_path(self.graph, id),
        }
    }

    /// The index's candidates are its global name table's callables named
    /// like `id`: here the clean base ones and the overlay's
    /// ([`OverlayFacts::global_named`]), with the metas the index reads.
    fn canonical_c_declaration(&self, id: u32) -> u32 {
        let facts = self.members.facts;
        if !may_be_declaration(facts, id, facts.c_callable(id)) {
            return id;
        }
        let Some(name) = TypeFacts::name_of(facts, id) else {
            return id;
        };
        let named = facts.global_named(name);
        canonical_declaration(
            facts,
            self.caller,
            self.source(),
            id,
            || {
                named
                    .iter()
                    .copied()
                    .filter(|&candidate| facts.kind_of(candidate).is_callable())
                    .map(|candidate| (candidate, facts.meta_of(candidate)))
            },
            |candidate| facts.c_callable(candidate),
        )
    }
}

/// The overlay's Rust module trees, laid out as the index lays out its own
/// (`RustWorkspaceModTree`): each Cargo target of the crate table
/// `graph.bin` stores (a crate's lib entry, then its bins) roots a tree of
/// file-backed `mod` declarations. Without a stored table (no
/// `Cargo.toml`) each Rust file roots a tree of its own `mod name;`
/// declarations, as in the index (`RustWorkspaceModTree::loose_scope`): the
/// file is the root, its children sit in its module directory, and `crate`
/// names nothing. Dirty declarations replace archived declarations,
/// including deletion. A base path the worktree removed is no file.
pub(super) struct RustModules<'a> {
    graph: &'a ArchivedZeroCopyGraph,
    /// Each dirty file with the virtual idx of its first symbol.
    dirty: FxHashMap<&'a str, (&'a OverlayFileInput, u32)>,
    /// The virtual idxs of the dirty declarations no module path names
    /// ([`DirtyRustFacts::body_locals`]).
    dirty_body_locals: FxHashSet<u32>,
    /// The virtual idxs the index tombstones ([`DirtyRustFacts::repeats`]).
    dirty_repeats: FxHashSet<u32>,
    /// Per base file index, the spans of its callables in a Rust file and
    /// none elsewhere, which tell a base body-local declaration
    /// ([`inside_callable`]). Built on the first such question about a Rust
    /// node.
    base_callables: OnceCell<Vec<Vec<Span>>>,
    /// Base node idx → the answer of its twin scan in
    /// [`Self::is_body_local`], which walks every node of its kind.
    base_twin_scans: RefCell<FxHashMap<u32, bool>>,
    removed: &'a FxHashSet<&'a str>,
    /// The `graph.bin` workspace facts, read on first use: reading them
    /// deserializes and validates the whole table, once per view.
    pub(super) crates: OnceCell<crate::contracts::workspace::WorkspaceFacts>,
    /// Whether the table holds no Cargo scope: every tree is manifest-less
    /// ([`Self::loose`]).
    loose: OnceCell<bool>,
    /// The current text of a module file, which its `pub use` rows are
    /// read from ([`Self::published`]). `None` for a view built without one.
    sources: Option<SourceReader<'a>>,
    /// Rust file → its text as the view reads it, read on the first
    /// question only that text can answer ([`Self::module_text`]): a
    /// `pub use` of the module, or a `mod` a file of its tree declares
    /// ([`Self::conventional_tree`]). The keys are set once per view, on
    /// that first question, so a module's rows borrow from the view. `None` for a text the
    /// source cannot give.
    module_texts: OnceCell<FxHashMap<&'a str, OnceCell<Option<ModuleText<'a>>>>>,
    /// Tree root file → `None` when the layout guess may place that tree's
    /// modules elsewhere than the index ([`Self::conventional_tree`]), else
    /// the index-only modules of the files the guess walks: parent file →
    /// the names it declares only where the parse makes no `Module` node.
    conventional_trees: RefCell<FxHashMap<&'a str, Option<IndexOnlyModules<'a>>>>,
}

/// Parent file → the names of the modules it declares that only the index
/// sees ([`RustModules::index_only_module`]).
type IndexOnlyModules<'a> = FxHashMap<&'a str, Vec<String>>;

/// One Rust module file's text and what the view reads from it.
struct ModuleText<'a> {
    text: Stripped,
    /// The file's `pub use` rows ([`rust_pub_use::pub_use_entries`]) per
    /// tree that holds it: the root file of the tree, and the rows read
    /// with the module's path in that tree ([`RustModules::pub_uses`]).
    trees: OnceCell<Vec<(&'a str, PubUseEntries)>>,
}

/// Where one caller file stands in the module trees
/// ([`RustModules::caller`]).
#[derive(Debug, Default)]
pub(super) struct RustCaller<'a> {
    /// The root file of the target whose `crate::` the caller writes.
    krate: Option<&'a str>,
    /// The root file of the target that declares the caller, and the
    /// caller's module path in it.
    module: Option<(&'a str, Vec<String>)>,
    /// Every target that declares the caller (the lib, each bin), with the
    /// caller's module path in it: what `crate::` / `self::` / `super::`
    /// module files must agree on ([`ladder::anchored_module_file`]).
    targets: Vec<(&'a str, Vec<String>)>,
}

impl<'a> RustModules<'a> {
    /// `facts` numbers `dirty`'s symbols in order after the graph's nodes.
    pub(super) fn new(
        graph: &'a ArchivedZeroCopyGraph,
        dirty: &'a [OverlayFileInput],
        removed: &'a FxHashSet<&'a str>,
        facts: DirtyRustFacts,
    ) -> Self {
        debug_assert_eq!(facts.starts.len(), dirty.len(), "one start per dirty file");
        Self {
            graph,
            dirty: dirty
                .iter()
                .zip(facts.starts)
                .map(|(file, start)| (file.rel_path.as_str(), (file, start)))
                .collect(),
            dirty_body_locals: facts.body_locals,
            dirty_repeats: facts.repeats,
            base_callables: OnceCell::new(),
            base_twin_scans: RefCell::default(),
            removed,
            crates: OnceCell::new(),
            loose: OnceCell::new(),
            sources: None,
            module_texts: OnceCell::new(),
            conventional_trees: RefCell::default(),
        }
    }

    /// The view's reader of a module file's current text.
    pub(super) fn with_sources(mut self, sources: Option<SourceReader<'a>>) -> Self {
        self.sources = sources;
        self
    }

    /// Whether no module path names `idx` (base or virtual) from outside
    /// its function: a scoped declaration, or a Rust one inside a function
    /// body. A node Pass 1 merged with a module-level twin is a module item.
    /// The index's `SymbolTable::is_body_local`.
    fn is_body_local(&self, idx: u32) -> bool {
        let graph = self.graph;
        if idx >= graph.nodes.len() as u32 {
            return self.dirty_body_locals.contains(&idx);
        }
        if graph.is_lexically_scoped(idx) {
            return true;
        }
        let node = &graph.nodes[idx as usize];
        let file_idx = node.file_idx.to_native();
        // The path test first: a repo without Rust never builds the table.
        if !is_rust_file(graph, file_idx as usize) {
            return false;
        }
        let callables = self
            .base_callables
            .get_or_init(|| base_rust_callables(graph));
        let inside = |span: Span| {
            callables
                .get(file_idx as usize)
                .is_some_and(|spans| inside_callable(spans, span))
        };
        if !inside(stored_span(node)) {
            return false;
        }
        let kind = NodeKind::from(&node.kind);
        // The index's per-node AND over the raws Pass 1 merged into `idx`.
        // Only a property, variable or constant merges a repeat
        // (`merges_repeats`), as a tombstone of the same kind and file with
        // the uid `tombstone(overload(uid, n))`; any other repeat is a node
        // of its own, so `idx` merged nothing.
        if !crate::uid::merges_repeats(kind) {
            return true;
        }
        if let Some(&body_local) = self.base_twin_scans.borrow().get(&idx) {
            return body_local;
        }
        let same_file = || {
            graph
                .nodes_by_kind(kind)
                .map(|i| &graph.nodes[i as usize])
                .filter(|twin| twin.file_idx.to_native() == file_idx)
        };
        let pool = &graph.string_pool;
        let (count, any_nameless) = same_file().fold((0u32, false), |(count, nameless), twin| {
            (count + 1, nameless || twin.name.resolve(pool).is_empty())
        });
        // A file with no tombstone (no name) of this kind merged nothing
        // into `idx`.
        let body_local = if !any_nameless {
            true
        } else {
            // A repeat's ordinal counts the earlier declarations of its uid,
            // so it stays below the file's count of this kind.
            let base = node.uid.to_native();
            let twin_uids: FxHashSet<u64> = (1..count)
                .map(|n| crate::uid::tombstone(crate::uid::overload(base, n)))
                .collect();
            same_file()
                .all(|twin| !twin_uids.contains(&twin.uid.to_native()) || inside(stored_span(twin)))
        };
        self.base_twin_scans.borrow_mut().insert(idx, body_local);
        body_local
    }

    /// Whether `idx` (base or virtual), whose owner is `owner_class`, is
    /// declared where a module path names it with an owner `owned` accepts:
    /// the one rule of the overlay's module-item lookups, the index's
    /// `SymbolTable::is_module_level`.
    fn module_level(
        &self,
        idx: u32,
        owner_class: Option<&str>,
        owned: impl FnOnce(Option<&str>) -> bool,
    ) -> bool {
        owned(owner_class.and_then(owner_key)) && !self.is_body_local(idx)
    }

    fn table(&self) -> &crate::contracts::workspace::WorkspaceFacts {
        self.crates.get_or_init(|| self.graph.workspace_facts())
    }

    /// Whether the index lays out manifest-less trees: the stored table
    /// holds no Cargo scope ([`WorkspaceFacts::has_cargo_scope`], the fact
    /// the index decides by).
    ///
    /// [`WorkspaceFacts::has_cargo_scope`]: crate::contracts::workspace::WorkspaceFacts::has_cargo_scope
    fn loose(&self) -> bool {
        *self.loose.get_or_init(|| !self.table().has_cargo_scope())
    }

    /// The entry file of the workspace crate that code names `head`: the
    /// index module tree's `<crate_name>::` head. Of two members with one
    /// code name, the table keeps the row of the one the index binds (the
    /// last listing). `None` for a name no row carries, and for a name two
    /// rows of one rank carry.
    pub(super) fn crate_head(&self, head: &str) -> Option<&'a str> {
        use crate::contracts::workspace::{Ecosystem, NameLookup};
        let crates = self.table();
        let NameLookup::Unique(id) = crates.lookup_name(Ecosystem::Cargo, head) else {
            return None;
        };
        let entry = crates.scope(id)?.entry_files.first()?;
        self.file(std::path::Path::new(entry))
    }

    pub(super) fn file(&self, path: &std::path::Path) -> Option<&'a str> {
        // `PathBuf::join` writes `\` on Windows; archived paths use `/`.
        let requested = os_path_to_slash(path.to_str()?);
        let requested = requested.as_ref();
        if let Some((&name, _)) = self.dirty.get_key_value(requested) {
            return Some(name);
        }
        if self.removed.contains(requested) {
            return None;
        }
        let graph = self.graph;
        let idx = graph.file_idx_of(requested)?;
        Some(graph.files[idx as usize].path.resolve(&graph.string_pool))
    }

    /// Whether `file` declares a module-level node named `name` whose kind
    /// `wanted` accepts ([`Self::module_level`]): an associated item
    /// (`impl X { const h: u32 = 1; }`) and a body-local declaration
    /// (`fn setup() { struct Foo; }`) are no items of the module. A dirty
    /// file answers from its fresh parse. Either side reads every named
    /// node, later nodes included, but no twin the index tombstones. That is
    /// the index's `SymbolTable::declares_in_file`, which reads a later node
    /// through its body-local first only ([`ladder::module_item`]): a later
    /// node has its first's kind, owner and file, so it adds an answer only
    /// where that first is body-local and it is not.
    fn declares(&self, file: &str, name: &str, wanted: impl Fn(NodeKind) -> bool) -> bool {
        let top_level = |owner: Option<&str>| owner.is_none();
        match self.dirty.get(file) {
            Some(&(dirty, start)) => dirty.symbols.iter().enumerate().any(|(at, s)| {
                let virt = start + at as u32;
                s.name == name
                    && wanted(s.kind)
                    && !self.dirty_repeats.contains(&virt)
                    && self.module_level(virt, s.owner_class.as_deref(), top_level)
            }),
            None => self.graph.nodes_by_name(name).any(|idx| {
                let node = &self.graph.nodes[idx as usize];
                let pool = &self.graph.string_pool;
                wanted(NodeKind::from(&node.kind))
                    && self.graph.files[node.file_idx.to_native() as usize]
                        .path
                        .resolve(pool)
                        == file
                    && self.module_level(idx, Some(node.owner_class.resolve(pool)), top_level)
            }),
        }
    }

    /// The file of the module `name` that `parent` declares (`mod name;`),
    /// as the index places it: in a manifest-less tree by the parent's text
    /// ([`rust_pub_use::loose_steps`]), so `None` without a source reader;
    /// in a Cargo tree by the layout ([`rust_pub_use::module_file`]).
    fn child(&self, parent: &'a str, name: &str, parent_is_root: bool) -> Option<&'a str> {
        if !self.declares(parent, name, |kind| kind == NodeKind::Module) {
            return None;
        }
        let file = |path: &str| self.file(std::path::Path::new(path));
        if self.loose() {
            let text = &self.module_text(parent)?.text;
            let (_, child) = rust_pub_use::loose_steps(text, parent, |path| file(path).is_some())
                .into_iter()
                .find(|(step, _)| *step == name)?;
            return file(&child);
        }
        rust_pub_use::module_file(std::path::Path::new(parent), parent_is_root, name, |p| {
            self.file(p)
        })
    }

    /// The module path of `caller` in the tree rooted at `root`: its path
    /// below the root's directory, each segment a module its parent
    /// declares. `None` when that walk does not reach the caller.
    fn module_path(&self, root: &'a str, caller: &str) -> Option<Vec<String>> {
        if root == caller {
            return Some(Vec::new());
        }
        let dir = std::path::Path::new(root).parent()?;
        let relative = std::path::Path::new(caller).strip_prefix(dir).ok()?;
        let mut module_path = relative.with_extension("");
        if module_path.file_name().is_some_and(|name| name == "mod") {
            module_path.pop();
        }
        let mut file = root;
        let mut segments = Vec::new();
        for part in module_path.components() {
            let name = part.as_os_str().to_str()?;
            file = self.child(file, name, segments.is_empty())?;
            segments.push(name.to_owned());
        }
        (file == caller).then_some(segments)
    }

    /// Where `caller` stands, by the index's rule: a file that one bin
    /// alone declares is in that bin. Otherwise `crate::` names the lib
    /// target of the named crate whose directory holds the caller (the
    /// deepest), and `self::` / `super::` start from the caller's module in
    /// that lib tree. Reads the crate table.
    pub(super) fn caller(&self, caller: &str) -> RustCaller<'a> {
        use crate::contracts::workspace::Ecosystem;
        if self.loose() {
            // `crate::` names nothing, and no target declares the file.
            let file = is_rust_source(std::path::Path::new(caller))
                .then(|| self.file(std::path::Path::new(caller)))
                .flatten();
            return RustCaller {
                module: file.map(|file| (file, Vec::new())),
                ..RustCaller::default()
            };
        }
        let crates = self.table();
        let mut reached: Vec<(&'a str, bool, Vec<String>)> = Vec::new();
        let mut home: Option<(usize, &'a str)> = None;
        for (id, scope) in crates.scopes.iter().enumerate() {
            if scope.ecosystem != Ecosystem::Cargo {
                continue;
            }
            for (at, entry) in scope.entry_files.iter().enumerate() {
                let Some(root) = self.file(std::path::Path::new(entry)) else {
                    continue;
                };
                if let Some(path) = self.module_path(root, caller) {
                    reached.push((root, at == 0, path));
                }
            }
            // The index keys its trees by crate name: a member another
            // listing shadows has a row but no name, and owns no file.
            let named = crates.names.iter().any(|name| name.scope.index() == id);
            let holds = scope.root.is_empty()
                || caller
                    .strip_prefix(scope.root.as_str())
                    .is_some_and(|rest| rest.starts_with('/'));
            if !named || !holds || home.is_some_and(|(depth, _)| depth >= scope.root.len()) {
                continue;
            }
            let lib = scope
                .entry_files
                .first()
                .and_then(|entry| self.file(std::path::Path::new(entry)));
            if let Some(lib) = lib {
                home = Some((scope.root.len(), lib));
            }
        }
        let lib = home.map(|(_, lib)| lib);
        let targets = reached
            .iter()
            .map(|(root, _, path)| (*root, path.clone()))
            .collect();
        let bin = match reached.as_slice() {
            [(root, false, path)] => Some((*root, path.clone())),
            _ => None,
        };
        RustCaller {
            krate: bin.as_ref().map(|&(root, _)| root).or(lib),
            module: bin.or_else(|| {
                let lib = lib?;
                reached
                    .into_iter()
                    .find(|&(root, primary, _)| primary && root == lib)
                    .map(|(root, _, path)| (root, path))
            }),
            targets,
        }
    }

    /// What `module` of `root` publishes `name` through, as the index
    /// answers (`IndexModuleTree::published`): the `pub use` naming `name`,
    /// else a glob `pub use`, else the module's own. A `name` the walk found
    /// to be a module only the index sees below `module`
    /// ([`Self::index_only_module`]) answers [`ladder::Published::Unknown`]
    /// first: the layout guess would read it as an item. A module none of
    /// whose import rows binds `name` or is a glob answers
    /// [`ladder::Published::Own`] at once, also where the layout guess may
    /// have placed a decoy file that is no module of the index
    /// (FU-2026-10-26-2ad63df39f50). So does a module whose text answers
    /// `Own` for a `name` it declares while no import row binds `name`, in
    /// any tree: the same gap. Otherwise the module's `pub use` rows
    /// ([`Self::pub_uses`]) answer when its text gives them and the layout
    /// guess places the tree's modules as the index does
    /// ([`Self::conventional_tree`]): the row's answer stands when the tree
    /// its target walk enters is such a tree too, and is
    /// [`ladder::Published::Unknown`] otherwise, since the rows show a
    /// re-export the overlay cannot follow. A `pub use` the parse makes no
    /// row for (one inside a macro body) is missed, a missing edge. Without
    /// those rows the import rows decide, which keep no visibility: a
    /// binding `use`, or a glob while the module declares no `name`, may be
    /// a `pub use` and answers [`ladder::Published::Unknown`]. So does a
    /// glob in a view with a source reader, whose rows were unreadable or
    /// whose tree the guess may misplace. Of these fallback answers, only a
    /// view without a reader answers [`ladder::Published::Own`] for a glob
    /// beside the module's own `name`.
    ///
    /// Accepted shape of the two `Own` shortcuts (FU-2026-10-26-2ad63df39f50):
    /// a file the index places under another module path, because a
    /// `#[path]` module reaches it first, answers `Own` for its own `name`
    /// at the path the layout guess gives it, where the index has no
    /// module.
    fn published(&self, root: &'a str, module: &'a str, name: &str) -> ladder::Published<'_> {
        if self.loose() {
            // A manifest-less tree keeps no `pub use` map.
            return ladder::Published::Own;
        }
        let Some(imports) = self.module_imports(module) else {
            return ladder::Published::Own;
        };
        let (mut binds, mut glob) = (false, false);
        for import in imports.iter() {
            binds |= import.alias.unwrap_or(import.imported_name) == name;
            glob |= import.imported_name == "*";
        }
        if self.index_only_module(root, module, name) {
            return ladder::Published::Unknown;
        }
        if !binds && !glob {
            return ladder::Published::Own;
        }
        if let Some(rows) = self.pub_uses(root, module) {
            let published = rust_pub_use::published(|name| rows.get(name), name);
            // A test module's private `use super::*;` makes its file a glob
            // module: answering its own item here keeps the tree walk off
            // it.
            if matches!(published, ladder::Published::Own)
                && !binds
                && self.declares(module, name, |_| true)
            {
                return published;
            }
            if self.conventional_tree(root) {
                return if !self.index_only_module(root, module, name)
                    && self.target_tree_conventional(root, &published)
                {
                    published
                } else {
                    ladder::Published::Unknown
                };
            }
        }
        if binds || self.sources.is_some() || !self.declares(module, name, |_| true) {
            ladder::Published::Unknown
        } else {
            ladder::Published::Own
        }
    }

    /// Whether the tree a re-export's target walk enters from `root` is one
    /// the layout guess places as the index does: `root` itself, or the
    /// workspace crate the prefix names, as `ladder`'s `tree_follow` picks
    /// it ([`ladder::reexport_anchor`]).
    fn target_tree_conventional(&self, root: &'a str, published: &ladder::Published<'_>) -> bool {
        let ladder::Published::ReExport(entry) = published else {
            return true;
        };
        let (tree, _, _) = ladder::reexport_anchor(root, entry, |head| self.crate_head(head));
        tree == root || self.conventional_tree(tree)
    }

    /// The `pub use` rows of the Rust file `module` in the tree rooted at
    /// `root`: the index's rows, read from the module's text
    /// ([`Self::module_text`]), each with the module's path in that tree as
    /// its containing path. `None` without the text, and when no tree of
    /// the root holds the module.
    fn pub_uses(&self, root: &'a str, module: &'a str) -> Option<&PubUseEntries> {
        let read = self.module_text(module)?;
        let trees = read.trees.get_or_init(|| {
            self.caller(module)
                .targets
                .into_iter()
                .map(|(tree, path)| (tree, rust_pub_use::pub_use_entries(&read.text, &path)))
                .collect()
        });
        trees
            .iter()
            .find(|(tree, _)| *tree == root)
            .map(|(_, rows)| rows)
    }

    /// Whether the layout guess ([`Self::child`]) places every module of the
    /// tree rooted at `root` it reaches where the index does. Decided once
    /// per root and view, on the first question whose text rows stand on it.
    ///
    /// The index walks the tree from its root (`build_mod_tree_and_reexports`):
    /// from each file it takes the steps [`rust_pub_use::mod_steps`] lists,
    /// and it places a file once, at the first step that reaches it. This
    /// walk takes every step of that walk over the view's files: each
    /// file's text ([`Self::module_text`]) gives the index's steps, and at a
    /// file the guess walks, the `Module` nodes the file declares (a dirty
    /// file's fresh parse, else `graph.bin`'s), each placed by
    /// [`Self::child`], give the guess's. The tree is placed like the index
    /// when at every file the guess walks each guess step is an index step,
    /// no file of the walk declares a `#[path]` module
    /// ([`rust_pub_use::path_attrs`]), and the walk reaches no file twice.
    /// An index step the guess lacks (a `mod` inside a macro call, which
    /// the parse makes no `Module` node for) starts a subtree the guess
    /// never enters, walked with the index's steps alone. Then every guess
    /// route from the root is an index route, and the index's walk drops
    /// none of its steps, so the guess places every module it reaches where
    /// the index does. `false` without a source, and when a file of the
    /// walk cannot be read.
    fn conventional_tree(&self, root: &'a str) -> bool {
        let known = self
            .conventional_trees
            .borrow()
            .get(root)
            .map(Option::is_some);
        if let Some(known) = known {
            return known;
        }
        let walked = self.walks_like_index(root);
        let conventional = walked.is_some();
        self.conventional_trees.borrow_mut().insert(root, walked);
        conventional
    }

    /// Whether `name` is a module the index places below `module` in the
    /// tree of `root` that the layout guess cannot see (a `mod` inside a
    /// macro call): the guess would read `name` as an item there. Known
    /// only once [`Self::conventional_tree`] walked the tree, so
    /// [`Self::published`] asks again after its own walk. An answer that
    /// needs no walk (no binding or glob row, or the `Own` shortcut) comes
    /// first in a tree no question walked yet: the accepted gap of a
    /// direct path through an index-only module.
    fn index_only_module(&self, root: &str, module: &str, name: &str) -> bool {
        self.conventional_trees
            .borrow()
            .get(root)
            .and_then(Option::as_ref)
            .and_then(|index_only| index_only.get(module))
            .is_some_and(|names| names.iter().any(|known| known == name))
    }

    /// The walk [`Self::conventional_tree`] decides by.
    fn walks_like_index(&self, root: &'a str) -> Option<IndexOnlyModules<'a>> {
        let graph = self.graph;
        let pool = &graph.string_pool;
        let mut base_modules: FxHashMap<&'a str, Vec<&'a str>> = FxHashMap::default();
        for idx in graph.nodes_by_kind(NodeKind::Module) {
            let node = &graph.nodes[idx as usize];
            let file = graph.files[node.file_idx.to_native() as usize]
                .path
                .resolve(pool);
            base_modules
                .entry(file)
                .or_default()
                .push(node.name.resolve(pool));
        }
        let declared = |file: &'a str| -> Vec<&'a str> {
            match self.dirty.get(file) {
                Some(&(input, _)) => input
                    .symbols
                    .iter()
                    .filter(|symbol| symbol.kind == NodeKind::Module)
                    .map(|symbol| symbol.name.as_str())
                    .collect(),
                None => base_modules.get(file).cloned().unwrap_or_default(),
            }
        };
        let mut index_only_modules = IndexOnlyModules::default();
        let mut visited: FxHashSet<&'a str> = FxHashSet::from_iter([root]);
        let mut stack = vec![(root, false)];
        while let Some((file, index_only)) = stack.pop() {
            let read = self.module_text(file)?;
            let attrs = rust_pub_use::path_attrs(&read.text);
            if !attrs.is_empty() {
                return None;
            }
            let at_root = file == root;
            let path = std::path::Path::new(file);
            let mut index: Vec<(&str, &'a str)> =
                rust_pub_use::mod_steps(&read.text, &attrs, path, at_root, |p| {
                    self.file(p).is_some()
                })
                .into_iter()
                .filter_map(|(name, child)| Some((name, self.file(&child)?)))
                .collect();
            index.sort_unstable();
            index.dedup();
            let mut guess: Vec<(&str, &'a str)> = Vec::new();
            if !index_only {
                guess.extend(
                    declared(file)
                        .into_iter()
                        .filter_map(|name| Some((name, self.child(file, name, at_root)?))),
                );
                guess.sort_unstable();
                guess.dedup();
            }
            if !guess.iter().all(|step| index.binary_search(step).is_ok()) {
                return None;
            }
            for step in index {
                if !visited.insert(step.1) {
                    return None;
                }
                let only_index = guess.binary_search(&step).is_err();
                if only_index && !index_only {
                    index_only_modules
                        .entry(file)
                        .or_default()
                        .push(step.0.to_owned());
                }
                stack.push((step.1, index_only || only_index));
            }
        }
        Some(index_only_modules)
    }

    /// The comment-stripped text of the Rust file `file`, read through
    /// [`Self::sources`] once per view ([`Self::module_texts`]). `None`
    /// without a source, for a file it cannot give, and for a path that is
    /// no Rust file of the view.
    fn module_text(&self, file: &'a str) -> Option<&ModuleText<'a>> {
        let sources = self.sources?;
        let graph = self.graph;
        let cells = self.module_texts.get_or_init(|| {
            let pool = &graph.string_pool;
            let base = graph
                .files
                .iter()
                .map(|file| file.path.resolve(pool))
                .filter(|path| !self.dirty.contains_key(path) && !self.removed.contains(path));
            self.dirty
                .keys()
                .copied()
                .chain(base)
                .filter(|path| is_rust_source(std::path::Path::new(path)))
                .map(|path| (path, OnceCell::new()))
                .collect()
        });
        cells
            .get(file)?
            .get_or_init(|| {
                Some(ModuleText {
                    text: rust_pub_use::strip_comments(&sources(file)?),
                    trees: OnceCell::new(),
                })
            })
            .as_ref()
    }

    /// The import rows of `module`: a dirty file's fresh parse, else the
    /// archived rows. `None` for a file the graph does not hold.
    fn module_imports(&self, module: &str) -> Option<FileImports<'a>> {
        let graph = self.graph;
        match self.dirty.get(module) {
            Some(&(file, _)) => Some(FileImports::Parsed(&file.imports)),
            None => graph
                .file_idx_of(module)
                .and_then(|idx| graph.file_imports(idx))
                .map(|rows| FileImports::Archived {
                    rows,
                    pool: graph.string_pool.as_slice(),
                    files: graph.files.as_slice(),
                }),
        }
    }

    /// The file of the module path `module` (`b::util`, `crate::a`) from
    /// the caller whose module chain is `chain`, root first: the view's
    /// tests read the trees through it.
    #[cfg(test)]
    pub(super) fn resolve(&self, chain: &[&'a str], module: &str) -> Option<&'a str> {
        let segment = |file: &&'a str| {
            let path = std::path::Path::new(*file);
            let stem = path.file_stem()?.to_str()?;
            match stem {
                "mod" => path.parent()?.file_name()?.to_str().map(str::to_owned),
                stem => Some(stem.to_owned()),
            }
        };
        let position = RustCaller {
            krate: chain.first().copied(),
            module: chain
                .first()
                .map(|&root| (root, chain[1..].iter().filter_map(segment).collect())),
            targets: Vec::new(),
        };
        let tree = OverlayTree {
            modules: self,
            caller: "",
            position: &position,
        };
        ladder::module_tree_module(&tree, "", module)
    }
}

/// [`RustModules`] read from one dirty caller ([`ladder::ModuleTreeFacts`]):
/// a root, a module and a file are all the module's stored path.
struct OverlayTree<'m, 'a> {
    modules: &'m RustModules<'a>,
    /// The caller `position` describes.
    caller: &'m str,
    position: &'m RustCaller<'a>,
}

impl<'a> OverlayTree<'_, 'a> {
    /// `f` over `caller`'s position: the one held for the dirty caller, or
    /// one read for another file.
    fn at<R>(&self, caller: &str, f: impl FnOnce(&RustCaller<'a>) -> R) -> R {
        if caller == self.caller {
            f(self.position)
        } else {
            f(&self.modules.caller(caller))
        }
    }
}

impl<'a> ladder::ModuleTreeFacts for OverlayTree<'_, 'a> {
    type Root = &'a str;
    type Module = &'a str;
    type File = &'a str;
    type Target = &'a str;

    fn crate_root(&self, name: &str) -> Option<&'a str> {
        self.modules.crate_head(name)
    }

    fn caller_crate(&self, caller: &str) -> Option<&'a str> {
        self.at(caller, |position| position.krate)
    }

    fn caller_module(&self, caller: &str) -> Option<(&'a str, Vec<String>)> {
        self.at(caller, |position| position.module.clone())
    }

    fn root_module(&self, root: &'a str) -> Option<&'a str> {
        Some(root)
    }

    fn submodule(&self, _root: &'a str, parent: &'a str, path: &[String]) -> Option<&'a str> {
        let (name, above) = path.split_last()?;
        // A manifest-less root is an ordinary file whose children sit in its
        // module directory.
        self.modules
            .child(parent, name, above.is_empty() && !self.modules.loose())
    }

    fn published(&self, root: &'a str, module: &'a str, name: &str) -> ladder::Published<'_> {
        self.modules.published(root, module, name)
    }

    fn declares(&self, module: &'a str, name: &str, namespace: ladder::Namespace) -> bool {
        self.modules
            .declares(module, name, |kind| namespace.holds(kind))
    }

    fn module_file(&self, module: &'a str) -> Option<&'a str> {
        Some(module)
    }

    fn declaring_targets(&self, caller: &str) -> Vec<(&'a str, Vec<String>)> {
        self.at(caller, |position| position.targets.clone())
    }

    fn target_module(&self, target: &'a str, path: &[String]) -> Option<&'a str> {
        let mut module = target;
        for at in 0..path.len() {
            module = self.submodule(target, module, &path[..=at])?;
        }
        Some(module)
    }
}

/// The constructor fallback's gate over the overlay
/// ([`ladder::CallFacts::declares_constructible`]): an overlay type or a
/// constructible base node is named `name`. Most unresolved calls name no
/// type at all, so this rejects them before [`name_tiers()`] collects
/// candidates.
fn names_constructible(
    graph: &ArchivedZeroCopyGraph,
    types: &OverlayNames<'_>,
    name: &str,
) -> bool {
    types.anywhere.contains_key(name)
        || graph
            .nodes_by_name(name)
            .any(|idx| NodeKind::from(&graph.nodes[idx as usize].kind).is_constructible())
}

/// A dirty file's memo of its Rust `::` path calls: `(callee, caller owner,
/// caller bases)` to the call's edge.
type ModuleCalls<'k> = FxHashMap<(&'k str, Option<&'k str>, *const String), Option<(u32, f32)>>;

/// The overlay's facts for one call site's entry ([`ladder::call`]): what
/// [`OverlayTiers`] reads for the dirty file `file_ord`, the caller, and
/// the edge pass's memos and gap notes.
struct OverlayCall<'v, 'a, 'n, 'm, 'k, G, A> {
    graph: &'v ArchivedZeroCopyGraph,
    file_ord: usize,
    file: &'v OverlayFileInput,
    caller: FileMeta,
    /// The caller's owner key and enclosing bases: with the callee, the key
    /// of a Rust path's memo.
    owner: Option<&'k str>,
    heritage: &'k [String],
    base_metas: &'v RefCell<&'m mut FxHashMap<usize, FileMeta>>,
    /// The callable, type and qualifier name tables.
    names: (
        &'v OverlayNames<'n>,
        &'v OverlayNames<'n>,
        &'v OverlayNames<'n>,
    ),
    nodes: &'v [ViewNode],
    replaced: &'v FxHashMap<u32, u32>,
    dirty_base: &'v FxHashSet<u32>,
    facts: &'v OverlayFacts<'a>,
    type_ctors: &'v FxHashMap<u32, Vec<u32>>,
    construction_targets: &'v RefCell<FxHashMap<u32, u32>>,
    module_calls: &'v RefCell<ModuleCalls<'k>>,
    note_gap: &'v G,
    csharp_admitted: &'v A,
}

impl<'v, 'a, 'm, G, A> OverlayCall<'v, 'a, '_, 'm, '_, G, A> {
    /// The name tiers' facts ([`OverlayTiers`]) over the declarations of
    /// `names`.
    fn tiers(&self, names: &'v OverlayNames<'v>) -> OverlayTiers<'v, 'a, 'm> {
        let (_, types, qualifiers) = self.names;
        OverlayTiers::new(
            self.graph,
            self.file_ord,
            self.caller,
            self.base_metas,
            (names, types, qualifiers),
            (self.nodes, self.replaced, self.dirty_base),
            self.facts,
        )
    }
}

impl<'a, 'k, G, A> ladder::CallFacts<'k> for OverlayCall<'_, 'a, '_, '_, 'k, G, A>
where
    G: Fn(&str, &str),
    A: Fn(u32, CallAccess<'_>) -> Option<u32>,
{
    type Modules = OverlayFacts<'a>;
    type Decls = OverlayFacts<'a>;
    const TYPE_NAME: TypeName = TypeName::LastSegment;

    fn modules(&self) -> &OverlayFacts<'a> {
        self.facts
    }

    fn decls(&self) -> &OverlayFacts<'a> {
        self.facts
    }

    fn type_candidates<'q>(
        &self,
        source: &str,
        receiver: &'q str,
        imports: &'q [RawImport],
    ) -> TypeCandidates<'q> {
        self.facts.type_candidates(source, receiver, imports)
    }

    /// The index's `IndexCall::ladder`: the name tiers ([`name_tiers()`]) for
    /// the typed callee, else the site's own name. A Rust path's answer is
    /// memoized per file. No Rust receiver is typed, so `site` is its lookup.
    fn ladder(
        &self,
        site: CallSite<'k>,
        typed: Option<&str>,
        call: CallPick<'_>,
        heritage: &[String],
    ) -> Resolution {
        let (callables, ..) = self.names;
        let imports = self.file.imports.as_slice();
        let policy = TierPolicy {
            target: ladder::call_target(self.caller.language, site),
            bind_import: !call.untyped,
            call: Some(call),
            heritage,
        };
        let tiers = self.tiers(callables);
        if self.caller.language == Language::Rust && site.name().contains("::") {
            let key = (site.name(), self.owner, self.heritage.as_ptr());
            let memo = self.module_calls.borrow().get(&key).copied();
            let hit = memo.unwrap_or_else(|| {
                let hit = name_tiers(&tiers, site.name(), imports, policy).map(TierHit::edge);
                self.module_calls.borrow_mut().insert(key, hit);
                hit
            });
            return Resolution::from_ladder(hit);
        }
        let lookup = typed.map_or(site, CallSite::Plain);
        let hit = name_tiers(&tiers, lookup.name(), imports, policy).map(TierHit::edge);
        // A hit in the caller's file came before the partial-type tier.
        let rel_path = self.file.rel_path.as_str();
        if self.caller.language == Language::CSharp
            && hit.is_none_or(|(idx, _)| self.facts.node_file(idx) != Some(rel_path))
        {
            if let (CallSite::Plain(name), Some(owner)) = (lookup, self.owner) {
                if self.facts.unseen_part_owns(rel_path, name, owner) {
                    (self.note_gap)(name, GAP_CS_PARTIAL_BARE_CALL);
                }
            }
        }
        Resolution::from_ladder(hit)
    }

    fn csharp_admitted(&self, id: u32, site: CallSite<'_>) -> Option<u32> {
        (self.csharp_admitted)(id, CallAccess::of(site, &self.file.rel_path, self.owner))
    }

    fn bare_member(
        &self,
        member: &str,
        target: ResolveTarget,
        heritage: &[String],
    ) -> Option<(u32, f32)> {
        let (callables, ..) = self.names;
        debug_assert!(
            target == callables.target,
            "a lookup of another kind family"
        );
        name_tiers(
            &self.tiers(callables),
            member,
            &self.file.imports,
            TierPolicy::lookup(self.caller.language, target, heritage),
        )
        .map(TierHit::edge)
    }

    fn names_type_and_member(&self, callee: &str) -> bool {
        self.facts.names_type_and_member(callee, self.caller)
    }

    fn declares_constructible(&self, name: &str) -> bool {
        names_constructible(self.graph, self.names.1, name)
    }

    fn resolve_type(
        &self,
        path: &str,
        bind_imports: bool,
        call: CallPick<'_>,
        heritage: &[String],
    ) -> Option<(u32, f32)> {
        let policy = TierPolicy {
            target: ResolveTarget::Type,
            bind_import: bind_imports && !call.untyped,
            call: Some(call),
            heritage,
        };
        name_tiers(&self.tiers(self.names.1), path, &self.file.imports, policy).map(TierHit::edge)
    }

    /// The import tiers alone ([`import_tiers`], the index's
    /// `resolve_import_tiers`).
    fn import_tiers(
        &self,
        name: &str,
        target: ResolveTarget,
        bind_imports: bool,
    ) -> Option<(u32, f32)> {
        let (callables, types, _) = self.names;
        let names = match target {
            ResolveTarget::Type => types,
            _ => callables,
        };
        import_tiers(
            &self.tiers(names),
            name,
            &self.file.imports,
            bind_imports,
            target,
        )
        .map(TierHit::edge)
    }

    fn construction_target(&self, type_id: u32) -> u32 {
        *self
            .construction_targets
            .borrow_mut()
            .entry(type_id)
            .or_insert_with(|| {
                construction_target(self.graph, self.nodes, self.type_ctors, type_id)
            })
    }
}

/// The values a bare variant name may denote (the index's
/// `values_named` candidates).
fn is_value(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::EnumVariant | NodeKind::Macro | NodeKind::Const | NodeKind::Variable
    )
}

/// The overlay's share of the index's tables that variant references read,
/// first declaration of each uid only, as the index registers them.
#[derive(Default)]
struct VariantTables<'a> {
    /// Names of the overlay's `EnumVariant`s (`SymbolTable::has_enum_variant`).
    names: FxHashSet<&'a str>,
    /// Name → overlay [`is_value`] declarations, for the bare-name pick.
    values: FxHashMap<&'a str, Vec<(u32, FileMeta)>>,
}

impl<'a> VariantTables<'a> {
    fn add(&mut self, node: &'a ViewNode, virt: u32, meta: FileMeta, first_of_uid: bool) {
        if node.later_overload || !first_of_uid || !is_value(node.kind) {
            return;
        }
        if node.kind == NodeKind::EnumVariant {
            self.names.insert(node.name.as_str());
        }
        self.values
            .entry(node.name.as_str())
            .or_default()
            .push((virt, meta));
    }
}

/// One dirty file's resolved variant references: a file names the same
/// enum, and often the same variant, at many sites.
#[derive(Default)]
struct VariantCache<'f> {
    /// Qualifier → the type it names.
    types: FxHashMap<&'f str, Option<(u32, f32)>>,
    /// Bare name → the variant it denotes.
    bare: FxHashMap<&'f str, Option<(u32, f32)>>,
}

/// The index's `resolve_variant_ref` (Pass 2 over `field_reads`): the
/// `EnumVariant` a reference of the dirty file `file` names, with the edge
/// confidence. A bare name takes [`ladder::bare_variant`] over the file's
/// declarations and the project's values; a qualified path takes
/// [`ladder::qualified_variant`], whose qualifier resolves as a type
/// ([`resolve_type`]). Both answers are cached per file in `cache`.
#[allow(clippy::too_many_arguments)]
fn resolve_variant_ref<'f>(
    graph: &ArchivedZeroCopyGraph,
    variant_ref: VariantRef<'f>,
    owner_class: Option<&'f str>,
    file_ord: usize,
    file: &'f OverlayFileInput,
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    (types, qualifiers): (&OverlayNames<'_>, &OverlayNames<'_>),
    variants: &VariantTables<'_>,
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    cache: &mut VariantCache<'f>,
) -> Option<(u32, f32)> {
    // `SymbolTable::has_enum_variant`: most captured paths name no variant.
    let has_variant = |name: &str| {
        variants.names.contains(name)
            || graph.nodes_by_name(name).any(|idx| {
                NodeKind::from(&graph.nodes[idx as usize].kind) == NodeKind::EnumVariant
                    && !dirty_base.contains(&idx)
            })
    };
    let Some(path) = variant_ref.split() else {
        let name = variant_ref.path;
        return *cache.bare.entry(name).or_insert_with(|| {
            ladder::bare_variant(
                facts,
                name,
                caller,
                has_variant,
                || {
                    let decls = facts.decls_in_file(&file.rel_path, name);
                    (!decls.is_empty()).then_some(decls)
                },
                || {
                    // The index's `SymbolTable::values_named`: the clean
                    // base values, then the overlay's.
                    let base: Vec<(u32, FileMeta)> = graph
                        .nodes_by_name(name)
                        .filter(|&idx| {
                            is_value(NodeKind::from(&graph.nodes[idx as usize].kind))
                                && !dirty_base.contains(&idx)
                                && !graph.is_later_overload(idx)
                        })
                        .map(|idx| (idx, base_file_meta(graph, base_metas, idx)))
                        .collect();
                    let overlay = variants.values.get(name).into_iter().flatten().copied();
                    base.into_iter().chain(overlay)
                },
            )
            .edge()
        });
    };
    ladder::qualified_variant(
        facts,
        caller.language,
        owner_class,
        path,
        has_variant,
        |qualifier| {
            *cache.types.entry(qualifier).or_insert_with(|| {
                resolve_type(
                    graph,
                    qualifier,
                    file_ord,
                    &file.imports,
                    caller,
                    base_metas,
                    (types, qualifiers),
                    nodes,
                    replaced,
                    dirty_base,
                    facts,
                )
            })
        },
    )
}

/// One heritage row of a dirty file, as the index's `pass1_9_supertypes`
/// writes it: the subject the heritage belongs to, one declared base
/// spelling, and the type that spelling resolves to with the resolver's
/// confidence (`None`: no project type).
struct HeritageRow<'f> {
    subject: u32,
    spelling: &'f str,
    base: Option<(u32, f32)>,
}

/// The index's supertypes pre-pass (`pass1_9_supertypes`) over the dirty
/// files, before any supertype exists: each heritage subject (a type, the
/// type an `Impl` names, the owner of a Rust trait-impl method) with each of
/// its bases resolved from the declaring file as the index's `resolve_base`
/// resolves them ([`resolve_base`]), one row per `(subject, spelling)` of a
/// file, in declared order. Cost: one type resolution per distinct subject
/// name and base spelling of a file.
#[allow(clippy::too_many_arguments)]
fn dirty_heritage_rows<'f>(
    graph: &ArchivedZeroCopyGraph,
    files: &'f [OverlayFileInput],
    nodes: &[ViewNode],
    facts: &OverlayFacts<'_>,
    names: (&OverlayNames<'_>, &OverlayNames<'_>),
    base_metas: &mut FxHashMap<usize, FileMeta>,
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
) -> Vec<HeritageRow<'f>> {
    let base_len = graph.nodes.len() as u32;
    let mut rows: Vec<HeritageRow<'f>> = Vec::new();
    let mut start = 0usize;
    for (file_ord, file) in files.iter().enumerate() {
        let first = start;
        start += file.symbols.len();
        if file.symbols.iter().all(|s| s.heritage.is_empty()) {
            continue;
        }
        let caller = file.file_meta();
        let is_rust = caller.language == Language::Rust;
        let mut resolve = |name: &str| {
            resolve_type(
                graph,
                name,
                file_ord,
                &file.imports,
                caller,
                base_metas,
                names,
                nodes,
                replaced,
                dirty_base,
                facts,
            )
        };
        let mut subjects: FxHashMap<&str, Option<u32>> = FxHashMap::default();
        let mut bases: FxHashMap<&str, Option<(u32, f32)>> = FxHashMap::default();
        // One `impl Trait for O` block repeats its heritage on every method,
        // and the index keeps one row per `(subject, base)` of a file.
        let mut pushed: FxHashSet<(u32, &str)> = FxHashSet::default();
        for (offset, sym) in file.symbols.iter().enumerate() {
            if sym.heritage.is_empty() {
                continue;
            }
            let virt = base_len + (first + offset) as u32;
            let subject = if sym.kind.is_type() {
                // A later same-uid twin is a tombstone the index names nothing by.
                facts
                    .dirty_by_name
                    .get(sym.name.as_str())
                    .is_some_and(|virts| virts.contains(&virt))
                    .then_some(virt)
            } else {
                let type_name = match sym.kind {
                    NodeKind::Impl => Some(sym.name.as_str()),
                    kind if is_rust && kind.is_callable() => sym.owner_class.as_deref(),
                    _ => None,
                };
                let Some(type_name) = type_name.filter(|name| !name.is_empty()) else {
                    continue;
                };
                *subjects
                    .entry(type_name)
                    .or_insert_with(|| resolve(type_name).map(|(id, _)| id))
            };
            let Some(subject) = subject else {
                continue;
            };
            for spelling in &sym.heritage {
                let spelling = spelling.as_str();
                if !pushed.insert((subject, spelling)) {
                    continue;
                }
                let base = *bases.entry(spelling).or_insert_with(|| {
                    resolve_base(facts, files, caller.language, spelling, &mut resolve)
                });
                rows.push(HeritageRow {
                    subject,
                    spelling,
                    base,
                });
            }
        }
    }
    rows
}

/// The index's Pass 2 heritage edges (`pass2_emit_node_edges`) of every
/// dirty declaration: for each base spelling it lists, in order, an edge to
/// the type the spelling resolves to ([`resolve_base`]) at the resolver's
/// confidence, `Implements` to an interface or a trait and `Extends` to
/// any other type ([`heritage_rel`]). The source is the declaration itself,
/// a Rust trait-impl method and a tombstoned twin included, as Pass 2 emits
/// from every raw node's slot. Runs once the dirty supertypes exist, as
/// Pass 2 runs after the pre-pass. Cost: one type resolution per distinct
/// base spelling of a file.
#[allow(clippy::too_many_arguments)]
fn dirty_heritage_edges(
    graph: &ArchivedZeroCopyGraph,
    files: &[OverlayFileInput],
    nodes: &[ViewNode],
    facts: &OverlayFacts<'_>,
    names: (&OverlayNames<'_>, &OverlayNames<'_>),
    base_metas: &mut FxHashMap<usize, FileMeta>,
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
) -> Vec<ViewEdge> {
    let base_len = graph.nodes.len() as u32;
    let mut edges: Vec<ViewEdge> = Vec::new();
    let mut start = 0usize;
    for (file_ord, file) in files.iter().enumerate() {
        let first = start;
        start += file.symbols.len();
        if file.symbols.iter().all(|s| s.heritage.is_empty()) {
            continue;
        }
        let caller = file.file_meta();
        let mut resolve = |name: &str| {
            resolve_type(
                graph,
                name,
                file_ord,
                &file.imports,
                caller,
                base_metas,
                names,
                nodes,
                replaced,
                dirty_base,
                facts,
            )
        };
        let mut bases: FxHashMap<&str, Option<(u32, f32)>> = FxHashMap::default();
        for (offset, sym) in file.symbols.iter().enumerate() {
            let source = base_len + (first + offset) as u32;
            for spelling in &sym.heritage {
                let spelling = spelling.as_str();
                let base = *bases.entry(spelling).or_insert_with(|| {
                    resolve_base(facts, files, caller.language, spelling, &mut resolve)
                });
                let Some((target, confidence)) = base else {
                    continue;
                };
                let rel_type = heritage_rel(facts.kind_of(target));
                edges.push(ViewEdge {
                    source,
                    target,
                    rel_type,
                    confidence,
                    reason: heritage_reason(rel_type),
                });
            }
        }
    }
    edges
}

/// The index's `resolve_base` for the base `spelling` of a declaration in a
/// dirty file of `language`, through that file's type lookup (`resolve`,
/// [`resolve_type`]). Outside C# it is the lookup's answer as is. A C#
/// spelling looks up its bare name ([`csharp_base_arity`]), takes the type
/// of its type-argument count ([`overlay_with_arity`]) and names a partial
/// type by its representative part. The index's last C# fallback is not
/// mirrored: a bare name whose global candidates are the parts of one
/// partial type binds nothing here.
fn resolve_base(
    facts: &OverlayFacts<'_>,
    files: &[OverlayFileInput],
    language: Language,
    spelling: &str,
    resolve: &mut impl FnMut(&str) -> Option<(u32, f32)>,
) -> Option<(u32, f32)> {
    if language != Language::CSharp {
        return resolve(spelling);
    }
    let (name, arity) = csharp_base_arity(spelling);
    let (id, confidence) = resolve(name)?;
    let id = overlay_with_arity(facts, files, id, arity)?;
    Some((TypeFacts::partial_type(facts, id), confidence))
}

/// The index's `SymbolTable::with_arity` for the type `id` a C# bare name
/// found: with no same-named type beside it in its file, `id` itself when
/// the spelling has no type arguments or `id` declares `arity`; else the
/// first of `id` and its same-file namesakes that declares `arity`.
/// `graph.bin` keeps no generic arity, so a clean `id` stands as it is.
fn overlay_with_arity(
    facts: &OverlayFacts<'_>,
    files: &[OverlayFileInput],
    id: u32,
    arity: u32,
) -> Option<u32> {
    let Some(mut offset) = id.checked_sub(facts.base_len()).map(|o| o as usize) else {
        return Some(id);
    };
    let file = files.iter().find(|file| {
        let here = offset < file.symbols.len();
        if !here {
            offset -= file.symbols.len();
        }
        here
    })?;
    let own = &file.symbols[offset];
    let twins: Vec<usize> = file
        .symbols
        .iter()
        .enumerate()
        .filter(|&(at, sym)| at != offset && sym.kind.is_type() && sym.name == own.name)
        .map(|(at, _)| at)
        .collect();
    if twins.is_empty() {
        return (arity == 0 || own.generic_arity == arity).then_some(id);
    }
    let first = id - offset as u32;
    std::iter::once(offset)
        .chain(twins)
        .find(|&at| file.symbols[at].generic_arity == arity)
        .map(|at| first + at as u32)
}

/// The index's `SymbolTable::build_supertypes` over the dirty files'
/// heritage `rows` ([`dirty_heritage_rows`]). A subject that is a clean
/// type keeps its archived bases too, less the traits of the dirty files'
/// archived `impl` blocks ([`OverlayFacts::base_supertypes`]): `graph.bin`
/// records no declaring file per base, only those blocks.
fn dirty_supertypes(
    facts: &OverlayFacts<'_>,
    base_len: u32,
    rows: &[HeritageRow<'_>],
) -> FxHashMap<u32, Supertypes> {
    let mut acc: FxHashMap<u32, Supertypes> = FxHashMap::default();
    for row in rows {
        let subject = row.subject;
        let entry = acc.entry(subject).or_insert_with(|| {
            (subject < base_len)
                .then(|| facts.base_supertypes(subject))
                .flatten()
                .unwrap_or_default()
        });
        match row.base {
            None if entry.1.is_none() => entry.1 = Some(entry.0.len() as u32),
            None => {}
            Some((id, _)) if id != subject && !entry.0.contains(&id) => entry.0.push(id),
            Some(_) => {}
        }
    }
    acc
}

/// The index's `resolve_symbol(.., ResolveTarget::Type)` for `name` in the
/// dirty file `file_ord`: the type tiers outside a call site, then for a
/// Rust path the module tree.
#[allow(clippy::too_many_arguments)]
fn resolve_type<'s>(
    graph: &ArchivedZeroCopyGraph,
    name: &'s str,
    file_ord: usize,
    imports: &'s [RawImport],
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    (types, qualifiers): (&OverlayNames<'_>, &OverlayNames<'_>),
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
) -> Option<(u32, f32)> {
    let base_metas = RefCell::new(base_metas);
    let tiers = OverlayTiers::new(
        graph,
        file_ord,
        caller,
        &base_metas,
        (types, types, qualifiers),
        (nodes, replaced, dirty_base),
        facts,
    );
    let policy = TierPolicy::lookup(caller.language, ResolveTarget::Type, &[]);
    name_tiers(&tiers, name, imports, policy).map(TierHit::edge)
}

/// The [`FileMeta`] of the base node `idx`'s file, memoized per file.
fn base_file_meta(
    graph: &ArchivedZeroCopyGraph,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    idx: u32,
) -> FileMeta {
    let file_idx = graph.nodes[idx as usize].file_idx.to_native() as usize;
    *base_metas
        .entry(file_idx)
        .or_insert_with(|| stored_file_meta(graph, file_idx as u32))
}

/// The qualifier tier's facts over the overlay ([`ladder::QualifierFacts`])
/// for a call of the dirty file `file_ord`: the view's facts, and the
/// qualifier-kind name table (`qualifiers`) for the names that file and the
/// overlay declare.
struct OverlayQualifiers<'v, 'a, 'n> {
    facts: &'v OverlayFacts<'a>,
    file_ord: usize,
    qualifiers: &'v OverlayNames<'n>,
}

impl<'a> ladder::ImportModules for OverlayQualifiers<'_, 'a, '_> {
    type Module = &'a str;

    fn import_module(
        &self,
        importer: &str,
        specifier: &str,
        language: Language,
    ) -> Module<&'a str> {
        self.facts.scope.module(importer, specifier, language)
    }

    fn module_name_indexed(&self, language: Language, name: &str) -> bool {
        self.facts.scope.has_module_name(language, name)
    }
}

impl OverlayQualifiers<'_, '_, '_> {
    /// The clean base nodes named `name` (no dirty-file node, no later
    /// overload) whose kind `wanted` accepts.
    fn clean_base<'s>(
        &'s self,
        name: &'s str,
        wanted: fn(NodeKind) -> bool,
    ) -> impl Iterator<Item = u32> + 's {
        let graph: &'s ArchivedZeroCopyGraph = self.facts.graph;
        let dirty_base: &'s FxHashSet<u32> = self.facts.dirty_base;
        graph.nodes_by_name(name).filter(move |&idx| {
            wanted(NodeKind::from(&graph.nodes[idx as usize].kind))
                && !dirty_base.contains(&idx)
                && !graph.is_later_overload(idx)
        })
    }

    /// Whether a declaration of a qualifier kind, clean base or overlay, is
    /// named `name`: the index's name table holds the same set.
    fn names_qualifier(&self, name: &str) -> bool {
        self.qualifiers.anywhere.contains_key(name)
            || self
                .clean_base(name, ResolveTarget::Qualifier.kind_predicate())
                .next()
                .is_some()
    }
}

impl<'a> ladder::QualifierFacts for OverlayQualifiers<'_, 'a, '_> {
    type Decls = OverlayFacts<'a>;

    fn decls(&self) -> &OverlayFacts<'a> {
        self.facts
    }

    fn caller_declares(&self, source: &str, name: &str, impl_receiver: bool) -> bool {
        debug_assert_eq!(source, self.facts.scope.dirty()[self.file_ord].rel_path);
        self.qualifiers.same_file.contains(&(self.file_ord, name))
            || impl_receiver
                && self
                    .qualifiers
                    .impl_same_file
                    .contains(&(self.file_ord, name))
                && !self.names_qualifier(name)
    }

    fn stored_file(&self, path: &str) -> Option<&'a str> {
        self.facts.stored_path(path)
    }

    /// The clean base declarations of a qualifier kind (no dirty-file node,
    /// no later overload) and the overlay's, through [`pick_global`]; with
    /// `impl_receiver` and none of them anywhere, the `impl` blocks through
    /// [`ladder::impl_qualifier_pick`].
    fn global_qualifier(
        &self,
        qualifier: &str,
        caller: FileMeta,
        impl_receiver: bool,
    ) -> GlobalPick {
        let facts = self.facts;
        if impl_receiver && !self.names_qualifier(qualifier) {
            let base = self
                .clean_base(qualifier, |kind| kind == NodeKind::Impl)
                .map(|idx| (idx, facts.meta_of(idx)));
            let overlay = self.qualifiers.impl_anywhere.get(qualifier);
            return ladder::impl_qualifier_pick(
                facts,
                caller,
                base.chain(overlay.into_iter().flatten().copied()),
            );
        }
        let base = self
            .clean_base(qualifier, ResolveTarget::Qualifier.kind_predicate())
            .map(|idx| (idx, facts.meta_of(idx)));
        let overlay = self
            .qualifiers
            .anywhere
            .get(qualifier)
            .into_iter()
            .flatten()
            .copied();
        pick_global(caller, base.chain(overlay))
    }

    fn for_each_candidate(
        &self,
        importer: &str,
        specifier: &str,
        mut visit: impl FnMut(&str) -> bool,
    ) {
        ModuleFacts::for_each_candidate(self.facts, importer, specifier, &mut visit);
    }

    fn exports(&self, file: &str, name: &str, target: ResolveTarget) -> bool {
        reexport::lookup_export_in_file(self.facts, file, name, target).is_some()
    }

    fn holds_import(&self, file: &str, imported: &str, default_import: bool) -> bool {
        reexport::imported_in_file(
            self.facts,
            file,
            imported,
            default_import,
            ResolveTarget::Qualifier,
        )
        .is_some()
    }

    fn rust_module_file(&self, source: &str, module_path: &str) -> Option<&'a str> {
        self.facts.rust_module_file(source, module_path)
    }

    /// A head that is neither anchored nor a module of the caller's file may
    /// name a workspace crate: the crate table and the module tree place it,
    /// as the index's do.
    fn crate_module(
        &self,
        source: &str,
        module_path: &str,
        member: &str,
        target: ResolveTarget,
    ) -> CrateModule<&'a str> {
        self.facts.with_rust_tree(self.file_ord, |tree| {
            ladder::crate_module(tree, self.facts, source, module_path, member, target)
        })
    }

    fn rust_path_agrees(
        &self,
        source: &str,
        callee: &str,
        qualifier: &str,
        candidate: &str,
        imports: &[RawImport],
    ) -> Option<bool> {
        self.facts.rust_type_path_agrees(
            (self.file_ord, source),
            Some(callee),
            qualifier,
            candidate,
            imports,
            self.qualifiers,
        )
    }

    fn files_by_stem(&self, stem: &str) -> impl Iterator<Item = &str> {
        self.facts.files_by_stem(stem).iter().copied()
    }

    fn type_candidates<'q>(
        &self,
        source: &str,
        name: &'q str,
        imports: &'q [RawImport],
    ) -> TypeCandidates<'q> {
        self.facts.type_candidates(source, name, imports)
    }
}

/// The import-member tier's facts over the overlay ([`ladder::ImportMembers`]):
/// module discovery is the view's [`ImportScope`]; a module file's member
/// is read from the name table of the lookup's kind family (`names`), and a
/// member import's type path from the type table (`types`).
struct OverlayImports<'v, 'a, 'n> {
    facts: &'v OverlayFacts<'a>,
    names: &'v OverlayNames<'n>,
    types: &'v OverlayNames<'n>,
}

impl<'a> ladder::ImportModules for OverlayImports<'_, 'a, '_> {
    type Module = &'a str;

    fn import_module(
        &self,
        importer: &str,
        specifier: &str,
        language: Language,
    ) -> Module<&'a str> {
        self.facts.scope.module(importer, specifier, language)
    }

    fn module_name_indexed(&self, language: Language, name: &str) -> bool {
        self.facts.scope.has_module_name(language, name)
    }
}

impl ladder::ImportMembers for OverlayImports<'_, '_, '_> {
    fn member_in_file(
        &self,
        file: &str,
        name: &str,
        target: ResolveTarget,
        owner: MemberOwner<'_>,
    ) -> Option<u32> {
        // A Rust method-syntax call looks up `Method` among the callables.
        debug_assert!(
            target == self.names.target
                || (target == ResolveTarget::Method
                    && self.names.target == ResolveTarget::Callable),
            "a lookup of another kind family"
        );
        let facts = self.facts;
        let owner_agrees = match owner {
            MemberOwner::Type(path) => owner_path_agrees_in_file(
                facts.graph,
                self.types,
                facts.nodes,
                facts.dirty_base,
                file,
                path,
            ),
            MemberOwner::Any | MemberOwner::TopLevel => true,
        };
        if !owner_agrees {
            return None;
        }
        member_in_file(
            facts.graph,
            self.names,
            facts.nodes,
            facts.dirty_base,
            facts,
            file,
            name,
            target,
            owner,
        )
    }

    fn python_reexport(
        &self,
        language: Language,
        module: &str,
        name: &str,
        target: ResolveTarget,
    ) -> Option<u32> {
        reexport::python_import_reexport(self.facts, language, module, name, target)
    }
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

/// The index's `lookup_member_in_file` over `target`'s kinds, read from
/// `names`, a table of those kinds or a superset: the first `member`
/// declared in `file` whose owner satisfies `owner`. A dirty file answers
/// from its fresh parse only.
#[allow(clippy::too_many_arguments)]
fn member_in_file(
    graph: &ArchivedZeroCopyGraph,
    names: &OverlayNames<'_>,
    nodes: &[ViewNode],
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    file: &str,
    member: &str,
    target: ResolveTarget,
    owner: MemberOwner<'_>,
) -> Option<u32> {
    if owner == MemberOwner::Any {
        // The match the same-file pick takes with no owner: a free
        // function, else one type's member.
        let pick = reexport::pick_call(facts.decls_in_file(file, member).into_iter(), target)?;
        return prefer_owned(
            facts,
            file,
            pick,
            member,
            None,
            &[],
            target,
            PickScope::default(),
        );
    }
    let owned = |found: Option<&str>| match owner {
        MemberOwner::Any => true,
        MemberOwner::TopLevel => found.is_none(),
        MemberOwner::Type(ty) => found.is_some_and(|key| Some(key) == owner_key(ty)),
    };
    let base_len = graph.nodes.len() as u32;
    graph
        .nodes_by_name(member)
        .filter(|&idx| {
            let node = &graph.nodes[idx as usize];
            target.kind_predicate()(NodeKind::from(&node.kind))
                && !dirty_base.contains(&idx)
                && graph
                    .files
                    .get(node.file_idx.to_native() as usize)
                    .is_some_and(|f| f.path.resolve(&graph.string_pool) == file)
                && facts.rust.module_level(
                    idx,
                    Some(node.owner_class.resolve(&graph.string_pool)),
                    owned,
                )
        })
        .min()
        .or_else(|| {
            // A later overload counts, as a base node does above: a
            // body-local first `new` must not hide a module-level one.
            names
                .anywhere
                .get(member)?
                .iter()
                .filter(|&&(virt, _)| nodes[(virt - base_len) as usize].rel_path.as_ref() == file)
                .flat_map(|&(virt, _)| std::iter::once(virt).chain(facts.later_overloads_of(virt)))
                .filter(|&virt| {
                    let node = &nodes[(virt - base_len) as usize];
                    target.kind_predicate()(node.kind)
                        && facts
                            .rust
                            .module_level(virt, node.owner_class.as_deref(), owned)
                })
                .min()
        })
}

/// The path aliases the index resolved with (tsconfig / jsconfig `paths`,
/// go.mod, Cargo.toml, ...), with one reading per stored C2 scope (R-A4b),
/// read once per view. A graph whose scope rows are all Cargo rows (the
/// Rust crate table, which this reading leaves alone) skips deserializing
/// them. With dirty resolution configs (R-A4d, `session::closure`),
/// `repo_wide` replaces the archived entries and `merged`, the archived rows
/// with the dirty configs' rows, replaces the stored rows.
fn overlay_aliases(
    graph: &ArchivedZeroCopyGraph,
    repo_wide: Option<PathAliases>,
    merged: Option<&crate::contracts::workspace::WorkspaceFacts>,
) -> PathAliases {
    let aliases = repo_wide.unwrap_or_else(|| graph.path_aliases());
    if let Some(facts) = merged {
        return aliases.with_scopes(facts);
    }
    let scoped = graph.workspace.scopes.iter().any(|scope| {
        !matches!(
            scope.ecosystem,
            crate::contracts::workspace::ArchivedEcosystem::Cargo
        )
    });
    if scoped {
        aliases.with_scopes(&graph.workspace_facts())
    } else {
        aliases
    }
}

/// The index's `crate_root_prefix` over a `/`-normalized path: everything
/// before the last `/src/` or `/tests/`, else empty.
fn crate_root_prefix(path: &str) -> &str {
    path.rsplit_once("/src/")
        .or_else(|| path.rsplit_once("/tests/"))
        .map_or("", |(root, _)| root)
}

/// The index's `leaf_path`: the last two components of `path`, its
/// directory's name and its file name (`collect/Maps.java`), or the whole
/// path at the root.
pub(super) fn leaf_path(path: &str) -> &str {
    let Some((dir, _)) = path.rsplit_once('/') else {
        return path;
    };
    dir.rfind('/').map_or(path, |slash| &path[slash + 1..])
}

/// The index's `path_dirs`: the directories of `path`, root first.
pub(super) fn path_dirs(path: &str) -> impl Iterator<Item = &str> {
    path.rsplit_once('/')
        .into_iter()
        .flat_map(|(dir, _)| dir.split('/'))
}

/// One dirty input's answers to the `Imports` derivation
/// ([`crate::analyzer::import_edges`]), as the index's `imports_edges` pass
/// answers them from its `Resolver`.
struct OverlayImportEdges<'q, 'a, 'n> {
    graph: &'a ArchivedZeroCopyGraph,
    file_ord: usize,
    file: &'a OverlayFileInput,
    /// The input's own meta, as its calls read it.
    caller: FileMeta,
    /// The importer's language as the index reads it (the first input of
    /// the path).
    language: Language,
    base_metas: &'q RefCell<&'q mut FxHashMap<usize, FileMeta>>,
    callables: &'q OverlayNames<'n>,
    types: &'q OverlayNames<'n>,
    qualifiers: &'q OverlayNames<'n>,
    nodes: &'a [ViewNode],
    replaced: &'a FxHashMap<u32, u32>,
    dirty_base: &'a FxHashSet<u32>,
    facts: &'q OverlayFacts<'a>,
    /// Each dirty path's `File` node, as the replay numbers it.
    dirty_files: &'q FxHashMap<&'a str, u32>,
    /// Steps 3c to 3f's index, built on the first import that reaches them.
    guesses: &'q OnceCell<GuessIndex<'a>>,
}

impl ImportEdgeFacts for OverlayImportEdges<'_, '_, '_> {
    fn named(
        &self,
        name: &str,
        target: ResolveTarget,
        binding_only: bool,
    ) -> (Vec<(u32, f32)>, Option<Arc<str>>) {
        let names = match target {
            ResolveTarget::Type => self.types,
            _ => self.callables,
        };
        let tiers = OverlayTiers {
            language: self.language,
            ..OverlayTiers::new(
                self.graph,
                self.file_ord,
                self.caller,
                self.base_metas,
                (names, self.types, self.qualifiers),
                (self.nodes, self.replaced, self.dirty_base),
                self.facts,
            )
        };
        let imports = self.file.imports.as_slice();
        // The index's `Resolver::resolve_import_binding` (the import tiers
        // alone) or `Resolver::resolve_import` (the whole ladder for a plain
        // name, no call site and no heritage).
        let hit = if binding_only {
            import_tiers(&tiers, name, imports, true, target)
        } else {
            let policy = TierPolicy::lookup(self.language, target, &[]);
            name_tiers(&tiers, name, imports, policy)
        };
        match hit {
            Some(hit) => (
                vec![(hit.id, hit.confidence)],
                hit.barrel.map(HitBarrel::into_arc),
            ),
            None => (Vec::new(), None),
        }
    }

    fn file_node(&self, path: &str) -> Option<u32> {
        self.facts.file_node(self.dirty_files, path)
    }

    fn for_each_candidate(&self, specifier: &str, visit: &mut dyn FnMut(&str) -> bool) {
        ModuleFacts::for_each_candidate(self.facts, &self.file.rel_path, specifier, visit);
    }

    fn guesses(&self) -> &GuessIndex<'_> {
        self.guesses
            .get_or_init(|| GuessIndex::new(self.facts.file_nodes(self.dirty_files)))
    }
}

/// The `Imports` edges of every dirty input, from its `File` node in
/// `dirty_files`, by the index's derivation ([`import_edges`]). Runs after
/// every call resolved: from here on candidate files are read without the
/// Rust module trees ([`OverlayFacts::tree_free`]). Cost per import: the
/// name ladder of a call, a few candidate probes (binary search over
/// `graph.files`, a name-index probe for a `File` node), and on the first
/// import that reaches a guess step one pass over the base `File` nodes.
#[allow(clippy::too_many_arguments)]
fn resolve_imports<'a>(
    graph: &'a ArchivedZeroCopyGraph,
    files: &'a [OverlayFileInput],
    nodes: &'a [ViewNode],
    replaced: &'a FxHashMap<u32, u32>,
    dirty_base: &'a FxHashSet<u32>,
    facts: &OverlayFacts<'a>,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    (callables, types, qualifiers): (&OverlayNames<'_>, &OverlayNames<'_>, &OverlayNames<'_>),
    dirty_files: &FxHashMap<&'a str, u32>,
) -> Vec<ViewEdge> {
    let mut edges = Vec::new();
    if files.iter().all(|file| file.imports.is_empty()) {
        return edges;
    }
    facts.tree_free.set(true);
    facts.forget_tree_answers();
    let base_metas = RefCell::new(&mut *base_metas);
    let guesses = OnceCell::new();
    for (file_ord, file) in files.iter().enumerate() {
        let path = file.rel_path.as_str();
        let Some(&source) = dirty_files.get(path) else {
            continue;
        };
        if file.imports.is_empty() {
            continue;
        }
        let language = facts.scope.file_meta(path).language;
        let lookup = OverlayImportEdges {
            graph,
            file_ord,
            file,
            caller: file.file_meta(),
            language,
            base_metas: &base_metas,
            callables,
            types,
            qualifiers,
            nodes,
            replaced,
            dirty_base,
            facts,
            dirty_files,
            guesses: &guesses,
        };
        import_edges(
            std::path::Path::new(path),
            source,
            language,
            &file.imports,
            &lookup,
            |edge| {
                edges.push(ViewEdge {
                    source,
                    target: edge.target,
                    rel_type: RelType::Imports,
                    confidence: edge.confidence,
                    reason: edge.reason.as_str(),
                })
            },
        );
    }
    edges
}

/// The `Decorates` pass of [`resolve_edges`] ([`resolve_decorates`]): what
/// it reads, and the edges and new `Annotation` nodes it hands the replay.
pub(super) struct Decorated<'s> {
    /// `(tombstone, survivor)` virtual indexes: a tombstoned twin's
    /// decorators source at its survivor, as the index's `RawIndexMap`
    /// merges the slot.
    pub survivors: &'s [(u32, u32)],
    /// The index the replay gives the first new `Annotation` node
    /// (`incremental::first_annotation`).
    pub first_annotation: u32,
    /// The dirty files' `Decorates` edges, each sourced at a virtual node.
    pub edges: Vec<ViewEdge>,
    /// The full name of each `Annotation` node the base lacks, the k-th
    /// taking `first_annotation + k`.
    pub annotations: Vec<String>,
}

/// The base `Annotation` node the index made for the decorator `full_name`
/// ([`decorates_edges`]): the one of that name with no owning file.
fn base_annotation(graph: &ArchivedZeroCopyGraph, full_name: &str) -> Option<u32> {
    graph.nodes_by_name(full_name).find(|&idx| {
        let node = &graph.nodes[idx as usize];
        NodeKind::from(&node.kind) == NodeKind::Annotation && !node.has_owning_file()
    })
}

/// The `Decorates` edges of every dirty declaration, by the index's
/// derivation ([`decorates_edges`]): a decorator name resolves as the
/// index's `Resolver::resolve_symbol` resolves it for a type, which is the
/// lookup of an `Imports` name ([`OverlayImportEdges`]), with the same
/// resolver (no Rust module tree). A name nothing declares reaches the base
/// `Annotation` node of its full name, or a new one numbered from
/// `first_annotation`, one per full name across the dirty files. Runs after
/// [`resolve_imports`]. Cost per decorator: the name ladder of an import
/// and, for an unresolved one, one name-index probe per distinct full name.
#[allow(clippy::too_many_arguments)]
fn resolve_decorates<'a>(
    graph: &'a ArchivedZeroCopyGraph,
    files: &'a [OverlayFileInput],
    nodes: &'a [ViewNode],
    replaced: &'a FxHashMap<u32, u32>,
    dirty_base: &'a FxHashSet<u32>,
    facts: &OverlayFacts<'a>,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    (callables, types, qualifiers): (&OverlayNames<'_>, &OverlayNames<'_>, &OverlayNames<'_>),
    dirty_files: &FxHashMap<&'a str, u32>,
    out: &mut Decorated<'_>,
) {
    if files
        .iter()
        .all(|file| file.symbols.iter().all(|s| s.decorators.is_empty()))
    {
        return;
    }
    if !facts.tree_free.replace(true) {
        facts.forget_tree_answers();
    }
    let Decorated {
        survivors,
        first_annotation,
        edges,
        annotations,
    } = out;
    let survivor_of: FxHashMap<u32, u32> = survivors.iter().copied().collect();
    let first_annotation = *first_annotation;
    let base_metas = RefCell::new(&mut *base_metas);
    let guesses = OnceCell::new();
    let mut seen: FxHashSet<(u32, u32)> = FxHashSet::default();
    let mut known: FxHashMap<String, u32> = FxHashMap::default();
    let mut file_start = graph.nodes.len() as u32;
    for (file_ord, file) in files.iter().enumerate() {
        let start = file_start;
        file_start += file.symbols.len() as u32;
        if file.symbols.iter().all(|s| s.decorators.is_empty()) {
            continue;
        }
        let lookup = OverlayImportEdges {
            graph,
            file_ord,
            file,
            caller: file.file_meta(),
            language: facts.scope.file_meta(file.rel_path.as_str()).language,
            base_metas: &base_metas,
            callables,
            types,
            qualifiers,
            nodes,
            replaced,
            dirty_base,
            facts,
            dirty_files,
            guesses: &guesses,
        };
        for (offset, symbol) in file.symbols.iter().enumerate() {
            let node = start + offset as u32;
            let source = survivor_of.get(&node).copied().unwrap_or(node);
            decorates_edges(
                source,
                symbol.kind,
                &symbol.decorators,
                &mut seen,
                |name| lookup.named(name, ResolveTarget::Type, false).0,
                |full_name| {
                    if let Some(&idx) = known.get(full_name) {
                        return idx;
                    }
                    let idx = base_annotation(graph, full_name).unwrap_or_else(|| {
                        annotations.push(full_name.to_owned());
                        first_annotation + annotations.len() as u32 - 1
                    });
                    known.insert(full_name.to_owned(), idx);
                    idx
                },
                |target, confidence| {
                    edges.push(ViewEdge {
                        source,
                        target,
                        rel_type: RelType::Decorates,
                        confidence,
                        reason: DECORATES_REASON,
                    })
                },
            );
        }
    }
}

/// What the shared `Overrides` derivation ([`crate::analyzer::overrides`])
/// reads over the overlay ([`resolve_overrides`]): the declared bases of
/// every type a dirty callable's lookup can reach, and the methods each of
/// those types declares.
struct OverrideView<'v, 'a> {
    facts: &'v OverlayFacts<'a>,
    bases: &'v FxHashMap<u32, Vec<BaseRow>>,
    methods: &'v MethodIndex<'v>,
}

impl OverrideFacts for OverrideView<'_, '_> {
    type Base = BaseRow;

    /// A part of a C# `partial` type with no rows of its own reads its
    /// representative's, as the index's `build_declared_bases` copies them
    /// to every part.
    fn declared_bases(&self, ty: u32) -> &[BaseRow] {
        self.bases
            .get(&ty)
            .or_else(|| self.bases.get(&self.partial_type(ty)))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn partial_type(&self, ty: u32) -> u32 {
        TypeFacts::partial_type(self.facts, ty)
    }

    fn node_kind(&self, id: u32) -> NodeKind {
        self.facts.kind_of(id)
    }

    fn methods<'s>(&'s self, ty: u32, name: &'s str) -> Option<&'s [Decl<'s>]> {
        self.methods.get(&(ty, name)).map(Vec::as_slice)
    }
}

/// The spelling of a gap among a clean type's bases ([`clean_bases`]):
/// `graph.bin` keeps no spelling, and a blank one is no base at all.
const UNSPELLED_BASE: &str = "?";

/// The declared bases of the clean type `ty` as the `Overrides` lookup
/// reads them from `graph.bin`: its archived supertypes
/// ([`OverlayFacts::base_supertypes`]) in declared order, each spelled by
/// its name and bound at the confidence of the type's archived `Extends` /
/// `Implements` edge to it (Pass 2 emits that edge from the resolution Pass
/// 1.9 recorded), and a gap where the first unresolved base stood. A base no
/// such edge reaches is a gap too: its confidence is unknown, and a guessed
/// one could make an edge the index does not have. Cost: the type's
/// out-edges.
fn clean_bases(facts: &OverlayFacts<'_>, ty: u32) -> Vec<BaseRow> {
    let Some((bases, first_unresolved)) = facts.base_supertypes(ty) else {
        return Vec::new();
    };
    let graph = facts.graph;
    let out = graph.out_offsets[ty as usize].to_native() as usize
        ..graph.out_offsets[ty as usize + 1].to_native() as usize;
    let mut bound: FxHashMap<u32, f32> = FxHashMap::default();
    for edge in &graph.edges[out] {
        if matches!(
            RelType::from(&edge.rel_type),
            RelType::Extends | RelType::Implements
        ) {
            let target = edge.target.to_native();
            let target = facts.replaced.get(&target).copied().unwrap_or(target);
            let known = bound.entry(target).or_insert(0.0);
            *known = known.max(edge.confidence.to_native());
        }
    }
    let gap = || BaseRow {
        spelling: UNSPELLED_BASE.to_owned(),
        candidates: Vec::new(),
    };
    let mut rows = Vec::with_capacity(bases.len() + 1);
    for (at, &base) in bases.iter().enumerate() {
        if first_unresolved == Some(at as u32) {
            rows.push(gap());
        }
        rows.push(match bound.get(&base) {
            Some(&confidence) => BaseRow {
                spelling: facts.name_of(base).to_owned(),
                candidates: vec![(base, confidence)],
            },
            None => gap(),
        });
    }
    if first_unresolved == Some(bases.len() as u32) {
        rows.push(gap());
    }
    rows
}

/// The archived meta of the clean callable `idx`, as the index paired it by
/// span: the flags and parameters the signature match reads.
fn archived_meta(graph: &ArchivedZeroCopyGraph, idx: u32) -> Option<RawFunctionMeta> {
    let meta = base_function_meta(graph, idx)?;
    let pool = &graph.string_pool;
    Some(RawFunctionMeta {
        span: (0, 0, 0, 0),
        flags: meta.flags.to_native(),
        params: meta
            .params
            .iter()
            .map(|param| param.resolve(pool).to_owned())
            .collect(),
        return_type: String::new(),
        decorators: Vec::new(),
        type_params: Vec::new(),
    })
}

/// Records the method `slot` named `name` under the type `key`, once per
/// node.
fn add_decl<'a>(
    methods: &mut MethodIndex<'a>,
    key: u32,
    name: &'a str,
    slot: u32,
    meta: Option<&'a RawFunctionMeta>,
) {
    let decls = methods.entry((key, name)).or_default();
    if !decls.iter().any(|d| d.slot == slot) {
        decls.push(Decl { slot, meta });
    }
}

/// The `Overrides` edges of every dirty callable, by the index's derivation
/// ([`crate::analyzer::overrides`]), with its confidence and reason.
///
/// A dirty callable's owner type comes from its file's fresh parse
/// ([`owner_types`]), its marker from [`OverlaySymbol::decorators`] and its
/// signature from [`OverlaySymbol::params`] and [`OverlaySymbol::flags`]; a
/// tombstoned twin looks up as its survivor (`survivors`), as the index's
/// `RawIndexMap` merges its slot. The rules come from the file's path, as
/// the index reads them. A dirty type's bases are its `heritage` rows, kept
/// as the index's `SymbolTable::build_declared_bases` keeps them; a clean
/// type's come from `graph.bin` ([`clean_bases`]), and its methods from its
/// archived `HasMethod` edges with their archived meta.
///
/// Cost: one owner sweep per dirty file (O(n log n) in its symbols); one
/// breadth-first walk from the dirty owner types over their bases, each
/// reached type once and at most `MAX_OVERRIDE_DEPTH + 2` levels, reading a
/// clean type's out-edges, archived supertypes and `PartOf` rows once; then
/// per dirty callable the lookup of [`method_lookup`], which per path walks
/// the owner's ancestors once (a Python order once per type). No name
/// lookup and no graph scan.
fn resolve_overrides(
    facts: &OverlayFacts<'_>,
    files: &[OverlayFileInput],
    heritage: &[HeritageRow<'_>],
    survivors: &[(u32, u32)],
) -> Vec<ViewEdge> {
    let graph = facts.graph;
    let base_len = facts.base_len();
    let file_rules: Vec<Option<Rules>> = files
        .iter()
        .map(|file| rules_for(file.file_meta().language))
        .collect();
    if file_rules.iter().all(Option::is_none) {
        return Vec::new();
    }
    let survivor_of: FxHashMap<u32, u32> = survivors.iter().copied().collect();
    let slot = |virt: u32| survivor_of.get(&virt).copied().unwrap_or(virt);
    let partial = |ty: u32| TypeFacts::partial_type(facts, ty);
    let owners: Vec<Vec<Option<usize>>> = files
        .iter()
        .map(|file| owner_types(&file.symbols))
        .collect();
    let dirty_metas: Vec<Vec<Option<RawFunctionMeta>>> = files
        .iter()
        .map(|file| {
            file.symbols
                .iter()
                .map(|symbol| {
                    symbol.kind.is_callable().then_some(())?;
                    Some(RawFunctionMeta {
                        span: HeritageNode::span(symbol),
                        flags: symbol.flags?,
                        params: symbol.params.clone(),
                        return_type: String::new(),
                        decorators: Vec::new(),
                        type_params: Vec::new(),
                    })
                })
                .collect()
        })
        .collect();

    // The dirty types' declared bases: under the representative part, the
    // declaring type never its own base, and a base whose one candidate is
    // the type dropped, as `build_declared_bases` keeps the pre-pass rows.
    // A clean representative keeps its archived bases first.
    let mut bases: FxHashMap<u32, Vec<BaseRow>> = FxHashMap::default();
    for row in heritage {
        if row.subject < base_len {
            continue;
        }
        let ty = partial(row.subject);
        let candidates: Vec<(u32, f32)> = row
            .base
            .map(|(id, confidence)| (partial(id), confidence))
            .into_iter()
            .filter(|&(id, _)| id != ty)
            .collect();
        if candidates.is_empty() && row.base.is_some() {
            continue;
        }
        let list = bases.entry(ty).or_insert_with(|| {
            if ty < base_len {
                clean_bases(facts, ty)
            } else {
                Vec::new()
            }
        });
        let base = BaseRow {
            spelling: row.spelling.to_owned(),
            candidates,
        };
        if !list.contains(&base) {
            list.push(base);
        }
    }

    // Every type a lookup can reach: the owner types of the dirty files
    // with rules, then their bound bases, level by level.
    let mut frontier: Vec<u32> = Vec::new();
    let mut start = base_len;
    for (file_ord, file) in files.iter().enumerate() {
        let first = start;
        start += file.symbols.len() as u32;
        if file_rules[file_ord].is_some() {
            frontier.extend(
                owners[file_ord]
                    .iter()
                    .flatten()
                    .map(|&owner| slot(first + owner as u32)),
            );
        }
    }
    let mut reached: Vec<u32> = Vec::new();
    let mut seen: FxHashSet<u32> = FxHashSet::default();
    for _ in 0..=MAX_OVERRIDE_DEPTH + 1 {
        let mut next: Vec<u32> = Vec::new();
        for ty in frontier {
            if !seen.insert(ty) {
                continue;
            }
            reached.push(ty);
            let key = if bases.contains_key(&ty) {
                ty
            } else {
                partial(ty)
            };
            if key < base_len && !bases.contains_key(&key) {
                bases.insert(key, clean_bases(facts, key));
            }
            for base in bases.get(&key).into_iter().flatten() {
                if let [(id, _)] = base.candidates.as_slice() {
                    next.push(*id);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    // The methods of each reached clean type, by its archived `HasMethod`
    // edges and those of its other clean parts, under the representative.
    let pool = &graph.string_pool;
    let mut archived: Vec<(u32, &str, u32, Option<RawFunctionMeta>)> = Vec::new();
    let mut scanned: FxHashSet<u32> = FxHashSet::default();
    for &ty in &reached {
        let group = facts.partial_group(ty);
        // A dirty type has no archived methods of its own, but a dirty part
        // of a partial type leaves its clean parts' methods to read.
        if ty >= base_len && group.is_empty() {
            continue;
        }
        let key = partial(ty);
        let parts: &[u32] = if group.is_empty() {
            std::slice::from_ref(&ty)
        } else {
            &group
        };
        for &part in parts {
            if part >= base_len || facts.dirty_base.contains(&part) || !scanned.insert(part) {
                continue;
            }
            let out = graph.out_offsets[part as usize].to_native() as usize
                ..graph.out_offsets[part as usize + 1].to_native() as usize;
            for edge in &graph.edges[out] {
                if RelType::from(&edge.rel_type) != RelType::HasMethod {
                    continue;
                }
                let method = edge.target.to_native();
                let node = &graph.nodes[method as usize];
                let name = node.name.resolve(pool).trim();
                if name.is_empty()
                    || !matches!(
                        NodeKind::from(&node.kind),
                        NodeKind::Method | NodeKind::Function
                    )
                    || facts.dirty_base.contains(&method)
                {
                    continue;
                }
                archived.push((key, name, method, archived_meta(graph, method)));
            }
        }
    }
    // The dirty methods by their owner types, as the index's
    // `build_method_index` keys them.
    let mut methods: MethodIndex<'_> = FxHashMap::default();
    let mut start = base_len;
    for (file_ord, file) in files.iter().enumerate() {
        let first = start;
        start += file.symbols.len() as u32;
        for (offset, symbol) in file.symbols.iter().enumerate() {
            let name = symbol.name.trim();
            if !matches!(symbol.kind, NodeKind::Method | NodeKind::Function) || name.is_empty() {
                continue;
            }
            let Some(owner) = owners[file_ord][offset] else {
                continue;
            };
            add_decl(
                &mut methods,
                partial(slot(first + owner as u32)),
                name,
                slot(first + offset as u32),
                dirty_metas[file_ord][offset].as_ref(),
            );
        }
    }
    for (key, name, method, meta) in &archived {
        add_decl(&mut methods, *key, name, *method, meta.as_ref());
    }

    let view = OverrideView {
        facts,
        bases: &bases,
        methods: &methods,
    };
    let mut mros = MroCache::default();
    let mut emitted = OverrideEdges::default();
    let mut start = base_len;
    for (file_ord, file) in files.iter().enumerate() {
        let first = start;
        start += file.symbols.len() as u32;
        let Some(rules) = file_rules[file_ord] else {
            continue;
        };
        for (offset, symbol) in file.symbols.iter().enumerate() {
            let Some(owner) = owners[file_ord][offset] else {
                continue;
            };
            let method = OverrideMethod {
                name: &symbol.name,
                kind: symbol.kind,
                decorators: &symbol.decorators,
                heritage: &symbol.heritage,
                meta: dirty_metas[file_ord][offset].as_ref(),
            };
            let owner = slot(first + owner as u32);
            let Some(lookup) = method_lookup(&view, rules, &method, owner, &mut mros) else {
                continue;
            };
            let source = slot(first + offset as u32);
            for &stop in &lookup.stops {
                if let Some(edge) = lookup.edge(source, stop) {
                    emitted.add(edge);
                }
            }
        }
    }
    emitted
        .into_edges()
        .into_iter()
        .map(|edge| ViewEdge {
            source: edge.source,
            target: edge.target,
            rel_type: RelType::Overrides,
            confidence: edge.confidence,
            reason: edge.reason,
        })
        .collect()
}

/// The `member_twins` table [`super::OverlayView::build_with_removed`] fills.
pub(super) type MemberTwins = FxHashMap<u32, (usize, Vec<(u32, Span)>)>;

/// The overlay's edges for every dirty file: `Calls`, lexical closure
/// `References`, name refs, enum-variant refs and field reads. `removed`
/// holds the base paths gone from the worktree: no module lookup finds one.
/// The `Imports` edges ([`resolve_imports`]) go to `imports` instead: a
/// re-parsed path's `File` node, their source, is a base index, which these
/// edges never have; `dirty_files` holds each dirty path's `File` node. The
/// `Decorates` edges ([`resolve_decorates`]) go to `decorated`: a new
/// `Annotation` node they reach is numbered past every virtual node yet.
/// The `Overrides` edges ([`resolve_overrides`]) go to `overrides`.
/// `configs` holds the dirty resolution configs' rows, which every module
/// lookup reads in place of the archived ones. `sources` reads a module
/// file's current text, which its Rust `pub use` rows come from
/// ([`RustModules::published`]).
/// Returns the lexical parent of each scoped virtual node.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_edges(
    graph: &ArchivedZeroCopyGraph,
    files: &[OverlayFileInput],
    base_len: u32,
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
    removed: &FxHashSet<&str>,
    type_ctors: &FxHashMap<u32, Vec<u32>>,
    later_overloads: &FxHashMap<u32, Vec<u32>>,
    member_twins: &MemberTwins,
    edges: &mut Vec<ViewEdge>,
    out_adj: &mut FxHashMap<u32, Vec<u32>>,
    in_adj: &mut FxHashMap<u32, Vec<u32>>,
    gaps: &RefCell<Vec<CallGap>>,
    (dirty_files, imports): (&FxHashMap<&str, u32>, &mut Vec<ViewEdge>),
    decorated: &mut Decorated<'_>,
    overrides: &mut Vec<ViewEdge>,
    configs: &ConfigEdits,
    sources: Option<SourceReader<'_>>,
) -> FxHashMap<u32, u32> {
    let mut callables = OverlayNames::new(ResolveTarget::Callable);
    let mut values = OverlayNames::new(ResolveTarget::Value);
    let mut fields = OverlayNames::new(ResolveTarget::Field);
    let mut types = OverlayNames::new(ResolveTarget::Type);
    let mut qualifiers = OverlayNames::new(ResolveTarget::Qualifier);
    // Base-file meta, filled on first sight: a common callee name has
    // hundreds of candidates spread over far fewer files.
    let mut base_metas: FxHashMap<usize, FileMeta> = FxHashMap::default();
    // Constructed type → its `Calls` target: a type is often
    // constructed at several sites, and a clean one costs name-index
    // probes.
    let construction_targets: RefCell<FxHashMap<u32, u32>> = RefCell::default();
    let file_metas: Vec<FileMeta> = files.iter().map(OverlayFileInput::file_meta).collect();

    // Full indexing tombstones a later same-uid declaration (a
    // half-finished edit) or keeps it as a later overload, and never
    // registers either in the global name table, so only the first
    // one of each uid is a cross-file candidate. The same-file tier
    // reads `OverlayFacts::declared`, which drops the later twin too.
    let mut named_uids: FxHashSet<u64> = FxHashSet::default();
    let mut variants = VariantTables::default();
    let mut virt_off = 0usize;
    for (file_ord, file) in files.iter().enumerate() {
        for _ in &file.symbols {
            let node = &nodes[virt_off];
            let virt = base_len + virt_off as u32;
            let first_of_uid = named_uids.insert(node.uid);
            callables.add(file_ord, node, virt, file_metas[file_ord], first_of_uid);
            values.add(file_ord, node, virt, file_metas[file_ord], first_of_uid);
            fields.add(file_ord, node, virt, file_metas[file_ord], first_of_uid);
            types.add(file_ord, node, virt, file_metas[file_ord], first_of_uid);
            qualifiers.add(file_ord, node, virt, file_metas[file_ord], first_of_uid);
            variants.add(node, virt, file_metas[file_ord], first_of_uid);
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
        debug_assert!(
            reason_index(edge.reason).is_some(),
            "VIEW_EDGE_REASONS must list every edge reason, or the view is never cached"
        );
        let ei = edges.len() as u32;
        edges.push(edge);
        out_adj.entry(edge.source).or_default().push(ei);
        in_adj.entry(edge.target).or_default().push(ei);
    };

    let facts = OverlayFacts::new(
        graph,
        files,
        nodes,
        dirty_base,
        removed,
        replaced,
        later_overloads,
        configs,
        sources,
    );
    let lexical_parents: FxHashMap<u32, u32> = facts.dirty_lexical_parents.clone();

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
    // The index's supertypes pre-pass, over the dirty files, before
    // any call resolves: the receiver ladder and the same-file pick
    // walk these bases, and the `Overrides` lookup reads the rows.
    let heritage = dirty_heritage_rows(
        graph,
        files,
        nodes,
        &facts,
        (&types, &qualifiers),
        &mut base_metas,
        replaced,
        dirty_base,
    );
    let _ = facts
        .dirty_supertypes
        .set(dirty_supertypes(&facts, base_len, &heritage));
    facts.forget_tree_answers();
    // Pass 2's `Extends` / `Implements` edges, once the bases exist.
    for edge in dirty_heritage_edges(
        graph,
        files,
        nodes,
        &facts,
        (&types, &qualifiers),
        &mut base_metas,
        replaced,
        dirty_base,
    ) {
        push_edge(edge);
    }
    // A value read inside `Foo1<T>` reads `Foo1<T>.foo`, the arity
    // twin of the `Foo1.foo` a lookup returns, when both sit in the
    // reader's file (the index's `SymbolTable::twin_read_from`).
    let twin_read_from =
        |target: u32, file_ord: usize, reader: Span| match member_twins.get(&target) {
            Some((ord, set)) if *ord == file_ord => {
                twin_enclosing(set.iter().copied(), reader).unwrap_or(target)
            }
            _ => target,
        };
    // B1 (the index's `MergedRows`): a merged row's edges start at its
    // survivor.
    let survivor_of: FxHashMap<u32, u32> = decorated.survivors.iter().copied().collect();
    for (file_ord, file) in files.iter().enumerate() {
        // The keys of the file's per-symbol edges, kept only for a file with
        // a merged row: its edge equal to one emitted already is dropped.
        let file_end = virt_cursor + file.symbols.len() as u32;
        let mut seen: Option<FxHashSet<(u32, u32, u8)>> = (virt_cursor..file_end)
            .any(|virt| survivor_of.contains_key(&virt))
            .then(FxHashSet::default);
        emit_closure_references(file, virt_cursor, &mut push_edge);
        // One cache per dirty file: a repeated qualified call reuses
        // both successful and unresolved lookups without rebuilding paths.
        let module_calls: RefCell<ModuleCalls<'_>> = RefCell::default();
        let mut variant_cache = VariantCache::default();
        // One type lookup per distinct annotation name of a file: the
        // lookup reads the file, never the declaration.
        let mut annotation_types: FxHashMap<&str, Option<(u32, f32)>> = FxHashMap::default();
        let caller = file_metas[file_ord];
        // Most files declare no base anywhere: their symbols skip the
        // enclosing-class scan.
        let any_heritage = file
            .symbols
            .iter()
            .any(|s| holds_heritage(s.kind) && !s.heritage.is_empty());
        let lexical = LexicalFunctions::new(
            std::path::Path::new(&file.rel_path),
            file.symbols
                .iter()
                .map(|s| (s.kind, s.name.as_str(), s.owner_class.is_some(), span_of(s))),
            virt_cursor,
        );
        for sym in &file.symbols {
            let source = virt_cursor;
            virt_cursor += 1;
            let from = survivor_of.get(&source).copied().unwrap_or(source);
            let mut emit = |edge: ViewEdge| {
                let fresh = seen.as_mut().is_none_or(|seen| {
                    seen.insert((edge.source, edge.target, edge.rel_type as u8))
                });
                if fresh || from == source {
                    push_edge(edge);
                }
            };
            let heritage: &[String] = if any_heritage {
                enclosing_class_heritage(sym, &file.symbols)
            } else {
                &[]
            };
            // A scoped caller reads the name tiers through the owner
            // of the unscoped declaration it sits in, as the index's
            // Pass 2 does (`SymbolTable::lexical_root`).
            let root = facts.lexical_root(source);
            let owner = match root == source {
                true => sym.owner_class.as_deref().and_then(owner_key),
                false => facts.owner_of(root),
            };
            // Duplicates go once the list is whole (`dedup_gaps`).
            let note_gap = |member: &str, reason: &str| {
                gaps.borrow_mut().push(CallGap {
                    path: file.rel_path.clone(),
                    line: sym.start_line,
                    caller: sym.name.clone(),
                    member: member.to_owned(),
                    reason: reason.to_owned(),
                });
            };
            // The index's C# access rule (codex ruling P-ext2 C) on
            // the bound target `idx`: `idx`, the one later overload
            // C# admits in its place, or `None`.
            let csharp_admitted = |idx: u32, access: CallAccess<'_>| {
                let flags = |id: u32| facts.csharp_method_flags(id);
                // The overload list is read only for a refused target.
                let picked = if csharp_access::call_admits(&facts, idx, flags(idx), access) {
                    Some(idx)
                } else {
                    csharp_access::admitted_overload(
                        &facts,
                        idx,
                        &facts.later_overloads_of(idx),
                        flags,
                        access,
                    )
                };
                // A dirty method with no flags of its own is a guess
                // (`csharp_method_flags`): a verdict that the
                // strictest flags would change rests on it.
                let guessed = |id: u32| {
                    !facts.csharp_flags_known(id)
                        && !csharp_access::call_admits(&facts, id, CS_STRICTEST_FLAGS, access)
                };
                if (picked != Some(idx) && !facts.csharp_flags_known(idx))
                    || picked.is_some_and(guessed)
                {
                    note_gap(facts.name_of(idx), GAP_CS_ACCESS_DIRTY_TARGET);
                }
                picked
            };
            for raw_callee in &sym.calls {
                // The index's Pass 2 order in a JS / TS or Rust file:
                // a bare call binds the innermost named function in
                // scope first, and an ambiguous or unreachable one leaves
                // only the import tiers (`CallContext::lexical`). A chain
                // walks from its innermost call outwards, as the index's
                // does.
                // Inline, so `parsed` borrows `sym.calls` for as long
                // as `module_calls` keeps its name.
                let mut chain_failed = false;
                // The two chain callbacks run sequentially and share
                // the same file-metadata cache.
                let chain_metas = std::cell::RefCell::new(&mut base_metas);
                let hit = CallSite::resolve_chain(
                    raw_callee,
                    |parsed, arguments| {
                        // The index's `Resolver::resolve_call`: the shared
                        // call entry, which reads the file's lexical verdict
                        // first (a function in scope binds, a hidden name
                        // takes the import-only path), then normalizes the
                        // site and derives the call pick from it.
                        let verdict = ladder::call(
                            &OverlayCall {
                                graph,
                                file_ord,
                                file,
                                caller,
                                owner,
                                heritage,
                                base_metas: &chain_metas,
                                names: (&callables, &types, &qualifiers),
                                nodes,
                                replaced,
                                dirty_base,
                                facts: &facts,
                                type_ctors,
                                construction_targets: &construction_targets,
                                module_calls: &module_calls,
                                note_gap: &note_gap,
                                csharp_admitted: &csharp_admitted,
                            },
                            CallContext {
                                source: &file.rel_path,
                                language: caller.language,
                                imports: &file.imports,
                                heritage,
                                owner,
                                caller: Some(source),
                                lexical: lexical.call_resolution(span_of(sym), parsed),
                            },
                            parsed,
                        );
                        let mut resolution = verdict.resolution;
                        // The index's field receiver of another `partial`
                        // part or a base
                        // (`Resolver::csharp_field_receiver_member`), ahead
                        // of the extension retry.
                        if resolution.call_fallbacks_allowed()
                            && caller.language == Language::CSharp
                        {
                            let access = CallAccess {
                                instance: true,
                                caller_file: &file.rel_path,
                                caller_owner: owner,
                            };
                            if let Some(hit) = facts
                                .csharp_field_receiver(&file.rel_path, owner, parsed, &types)
                                .and_then(|(id, confidence)| {
                                    Some((csharp_admitted(id, access)?, confidence))
                                })
                            {
                                resolution = Resolution::from_ladder(Some(hit));
                            }
                        }
                        // The index's C# extension retry of a receiver
                        // the parser typed (`Type.member`) or left
                        // untyped. It runs after `resolve_call` there, so
                        // no rule above judges its target. No C# site it
                        // reads constructs a type, so the constructor
                        // fallback before it bound nothing.
                        if resolution.call_fallbacks_allowed()
                            && !verdict.typed
                            && caller.language == Language::CSharp
                        {
                            if let Some((receiver, member)) = match verdict.site {
                                // `string.Format(..)` calls a static
                                // member of a predefined type: no
                                // value receives an extension.
                                CallSite::Plain(callee) => callee
                                    .rsplit_once('.')
                                    .filter(|&(receiver, _)| {
                                        !csharp_extension::is_keyword_type(receiver)
                                    })
                                    .map(|(receiver, member)| {
                                        (ReceiverSite::Named(receiver), member)
                                    }),
                                CallSite::UntypedMember(member) => {
                                    Some((ReceiverSite::Untyped, member))
                                }
                                _ => None,
                            } {
                                let hit = facts
                                    .csharp_extension_edge(
                                        &file.rel_path,
                                        receiver,
                                        member,
                                        arguments,
                                        &file.imports,
                                        owner,
                                    )
                                    .unwrap_or_else(|DirtyNamesake| {
                                        note_gap(member, GAP_CS_EXTENSION_DIRTY_DECLARATION);
                                        None
                                    });
                                resolution = Resolution::from_ladder(hit);
                            }
                        }
                        resolution.edge()
                    },
                    |inner, member, arguments| {
                        if chain_failed {
                            return Some(None);
                        }
                        let inner = (*inner)?;
                        let outcome =
                            facts.chained_member_outcome(inner, member, caller.language, &types);
                        // The index's chain access rule
                        // (`Resolver::csharp_admitted_chain_member`):
                        // the returned value is an instance, and a
                        // member C# refuses leaves the type's lookup
                        // empty, so the extension retry runs.
                        let outcome = match (outcome, caller.language) {
                            (
                                type_ladder::ChainOutcome::Bound((id, confidence)),
                                Language::CSharp,
                            ) => {
                                let access = CallAccess {
                                    instance: true,
                                    caller_file: &file.rel_path,
                                    caller_owner: owner,
                                };
                                match csharp_admitted(id, access) {
                                    Some(id) => type_ladder::ChainOutcome::Bound((id, confidence)),
                                    None => type_ladder::ChainOutcome::Conclusive,
                                }
                            }
                            (outcome, _) => outcome,
                        };
                        match outcome {
                            type_ladder::ChainOutcome::Unknown => None,
                            type_ladder::ChainOutcome::Bound(target) => Some(Some(target)),
                            type_ladder::ChainOutcome::Conclusive
                                if caller.language == Language::CSharp =>
                            {
                                let target = facts
                                    .csharp_extension_edge(
                                        &file.rel_path,
                                        ReceiverSite::Returned(inner.0),
                                        member,
                                        arguments,
                                        &file.imports,
                                        owner,
                                    )
                                    .unwrap_or_else(|DirtyNamesake| {
                                        note_gap(member, GAP_CS_EXTENSION_DIRTY_DECLARATION);
                                        None
                                    })
                                    .map(|(id, confidence)| (id, confidence.min(inner.1)));
                                chain_failed = target.is_none();
                                Some(target)
                            }
                            type_ladder::ChainOutcome::Conclusive => {
                                let target = if caller.language == Language::Kotlin {
                                    let tiers = OverlayTiers::new(
                                        graph,
                                        file_ord,
                                        caller,
                                        &chain_metas,
                                        (&callables, &types, &qualifiers),
                                        (nodes, replaced, dirty_base),
                                        &facts,
                                    );
                                    name_tiers::extension_fallback(
                                        &tiers,
                                        member,
                                        &file.imports,
                                        ResolveTarget::Callable,
                                        heritage,
                                    )
                                    .map(|(id, confidence)| (id, confidence.min(inner.1)))
                                } else {
                                    None
                                };
                                chain_failed = target.is_none();
                                Some(target)
                            }
                        }
                    },
                );
                // The index drops self-recursion Calls (builder's
                // `target_id == current_node_idx` skip).
                if let Some((target, confidence)) = hit.filter(|&(t, _)| t != from) {
                    // The index's Pass 2 names the reason by the same
                    // confidence (`CallReasons::of`).
                    let reason = if confidence == NEAREST_COPY_CONFIDENCE {
                        NEAREST_COPY_REASON
                    } else if confidence == UNTYPED_NAME_CONFIDENCE {
                        UNTYPED_NAME_REASON
                    } else if confidence == csharp_extension::HEURISTIC_CONFIDENCE {
                        csharp_extension::HEURISTIC_REASON
                    } else {
                        super::super::merged::OVERLAY_EDGE_REASON
                    };
                    emit(ViewEdge {
                        source: from,
                        target,
                        rel_type: RelType::Calls,
                        confidence,
                        reason,
                    });
                }
            }

            // The index's Pass 2 `type_annotation` edges, after its calls.
            let type_params = type_ladder::declaration_type_params(
                &facts,
                source,
                &sym.type_params,
                sym.decorators.as_slice(),
                |id| Some(facts.dirty_symbol(id)?.decorators.as_slice()),
            );
            let decl = AnnotatedDecl {
                language: caller.language,
                kind: sym.kind,
                type_annotation: sym.type_annotation.as_deref(),
                owner_class: sym.owner_class.as_deref(),
                return_type: sym.return_type.as_deref(),
                params: &sym.params,
                type_params: &type_params,
            };
            for (target, confidence, reason) in type_annotation_targets(decl, |name| {
                *annotation_types.entry(name).or_insert_with(|| {
                    resolve_type(
                        graph,
                        name,
                        file_ord,
                        &file.imports,
                        caller,
                        &mut base_metas,
                        (&types, &qualifiers),
                        nodes,
                        replaced,
                        dirty_base,
                        &facts,
                    )
                })
            }) {
                emit(ViewEdge {
                    source: from,
                    target,
                    rel_type: RelType::Accesses,
                    confidence,
                    reason: reason.reason(),
                });
            }

            // The index's Pass 2 `field_reads` loop. `variant_refs`
            // is its legacy field name: v5 preserves every raw read,
            // including tagged name references and ordinary fields.
            let mut emitted: Vec<(u32, bool)> = Vec::new();
            let mut name_edges: Vec<u32> = Vec::new();
            let mut field_edges: Vec<u32> = Vec::new();
            for raw in &sym.variant_refs {
                let read_owner = match root == source {
                    true => sym.owner_class.as_deref(),
                    false => owner,
                };
                if let Some(read) = MemberRead::parse(raw) {
                    // The index's member-read branch: the same lookups,
                    // then the target decides the edge.
                    let Some((target, confidence, edge)) = with_overlay_name_lookups(
                        graph,
                        file_ord,
                        file,
                        caller,
                        &mut base_metas,
                        &callables,
                        &values,
                        &fields,
                        &types,
                        &qualifiers,
                        nodes,
                        replaced,
                        dirty_base,
                        &facts,
                        |resolve, imported| {
                            resolve_member_read(
                                &facts,
                                &file.rel_path,
                                caller.language,
                                &file.imports,
                                read_owner,
                                read,
                                resolve,
                                imported,
                                |id| overlay_decorators(&facts, id),
                            )
                        },
                    ) else {
                        continue;
                    };
                    if target == from || name_edges.contains(&target) {
                        continue;
                    }
                    name_edges.push(target);
                    emit(ViewEdge {
                        source: from,
                        target,
                        rel_type: edge.rel_type(),
                        confidence,
                        reason: edge.reason(),
                    });
                    continue;
                }
                if let Some(name_ref) = NameRef::parse(raw) {
                    // The index's lexical name-reference tier, then
                    // its name tiers with the same owner.
                    let Some((target, confidence)) = with_overlay_name_lookups(
                        graph,
                        file_ord,
                        file,
                        caller,
                        &mut base_metas,
                        &callables,
                        &values,
                        &fields,
                        &types,
                        &qualifiers,
                        nodes,
                        replaced,
                        dirty_base,
                        &facts,
                        |resolve, imported| {
                            ladder::name_ref(
                                &facts,
                                &facts,
                                &file.rel_path,
                                caller.language,
                                &file.imports,
                                read_owner,
                                name_ref,
                                lexical.name_ref_target(span_of(sym), name_ref),
                                resolve,
                                imported,
                            )
                        },
                    ) else {
                        continue;
                    };
                    let callable = facts.kind(target).is_callable();
                    // The index's `SymbolTable::twin_read_from`.
                    let target = match callable {
                        true => target,
                        false => twin_read_from(target, file_ord, HeritageNode::span(sym)),
                    };
                    if target == from || name_edges.contains(&target) {
                        continue;
                    }
                    name_edges.push(target);
                    let (rel_type, reason) = if callable {
                        (RelType::References, FUNCTION_REFERENCE_REASON)
                    } else {
                        (RelType::ReadsField, VALUE_READ_REASON)
                    };
                    emit(ViewEdge {
                        source: from,
                        target,
                        rel_type,
                        confidence,
                        reason,
                    });
                    continue;
                }
                if let Some(variant_ref) = VariantRef::parse(raw) {
                    let Some((target, confidence)) = resolve_variant_ref(
                        graph,
                        variant_ref,
                        sym.owner_class.as_deref(),
                        file_ord,
                        file,
                        caller,
                        &mut base_metas,
                        (&types, &qualifiers),
                        &variants,
                        nodes,
                        replaced,
                        dirty_base,
                        &facts,
                        &mut variant_cache,
                    ) else {
                        continue;
                    };
                    if target == from || emitted.contains(&(target, variant_ref.pattern)) {
                        continue;
                    }
                    emitted.push((target, variant_ref.pattern));
                    emit(ViewEdge {
                        source: from,
                        target,
                        rel_type: RelType::References,
                        confidence,
                        reason: if variant_ref.pattern {
                            VARIANT_PATTERN_REASON
                        } else {
                            VARIANT_USE_REASON
                        },
                    });
                    continue;
                }
                // The index's `Resolver::resolve_field_read`.
                let field_metas = RefCell::new(&mut base_metas);
                let tiers = || {
                    OverlayTiers::new(
                        graph,
                        file_ord,
                        caller,
                        &field_metas,
                        (&fields, &types, &qualifiers),
                        (nodes, replaced, dirty_base),
                        &facts,
                    )
                };
                let hit = match ladder::field_read(&facts, &file.rel_path, raw) {
                    FieldRead::Bound(hit) => Some((hit.id, hit.confidence)),
                    FieldRead::Qualified => {
                        let policy = TierPolicy::lookup(caller.language, ResolveTarget::Field, &[]);
                        name_tiers(&tiers(), raw, &file.imports, policy).map(TierHit::edge)
                    }
                    FieldRead::Global => tiers()
                        .global(
                            raw,
                            &file.imports,
                            ResolveTarget::Field,
                            None,
                            &[],
                            SiteForm::GlobalFieldRead,
                        )
                        .edge(),
                };
                let Some((target, confidence)) = hit else {
                    continue;
                };
                let target = twin_read_from(target, file_ord, HeritageNode::span(sym));
                if target == from || field_edges.contains(&target) {
                    continue;
                }
                field_edges.push(target);
                emit(ViewEdge {
                    source: from,
                    target,
                    rel_type: RelType::ReadsField,
                    confidence,
                    reason: "field_read",
                });
            }
        }
    }
    *imports = resolve_imports(
        graph,
        files,
        nodes,
        replaced,
        dirty_base,
        &facts,
        &mut base_metas,
        (&callables, &types, &qualifiers),
        dirty_files,
    );
    resolve_decorates(
        graph,
        files,
        nodes,
        replaced,
        dirty_base,
        &facts,
        &mut base_metas,
        (&callables, &types, &qualifiers),
        dirty_files,
        decorated,
    );
    *overrides = resolve_overrides(&facts, files, &heritage, decorated.survivors);
    lexical_parents
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_fixture::GraphFixture;
    use rkyv::rancor::Error as RkyvError;

    /// Stands for: the overlay's name tables hold what the index's hold
    /// after Pass 1, a merged repeat never. In `C.cs`, `C.x`, the arity
    /// sibling `C<T>.x` (a later node) and a repeat merged into the sibling,
    /// which carries the sibling's uid while the tables are built, as
    /// `OverlayView::build` numbers them. Only `C.x` is a name candidate. Red
    /// before: the sibling's uid was never seen, so the repeat entered.
    #[test]
    fn test_overlay_facts_repeat_merged_into_arity_sibling_not_in_name_table() {
        let bytes = GraphFixture::new().into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let symbol = |line: u32| OverlaySymbol {
            is_exported: false,
            name: "x".to_string(),
            kind: NodeKind::Property,
            owner_class: Some("C".to_string()),
            start_line: line,
            end_line: line,
            start_column: 0,
            end_column: 0,
            calls: Vec::new(),
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
            decorators: Vec::new(),
            params: Vec::new(),
            type_annotation: None,
            type_params: Vec::new(),
        };
        let file = OverlayFileInput {
            parsed_language: None,
            rel_path: "C.cs".to_string(),
            symbols: vec![symbol(2), symbol(6), symbol(8)],
            imports: Vec::new(),
            export_aliases: Vec::new(),
        };
        let node = |uid: u64, line: u32, later_overload: bool| ViewNode {
            uid,
            name: "x".to_string(),
            kind: NodeKind::Property,
            owner_class: Some("C".to_string()),
            rel_path: Arc::from("C.cs"),
            start_line: line,
            end_line: line,
            start_column: 0,
            end_column: 0,
            replaced_base: None,
            later_overload,
        };
        let nodes = [node(11, 2, false), node(22, 6, true), node(22, 8, false)];
        let (dirty_base, removed) = (FxHashSet::default(), FxHashSet::default());
        let (replaced, later_overloads) = (FxHashMap::default(), FxHashMap::default());
        let configs = ConfigEdits::default();
        let facts = OverlayFacts::new(
            graph,
            std::slice::from_ref(&file),
            &nodes,
            &dirty_base,
            &removed,
            &replaced,
            &later_overloads,
            &configs,
            None,
        );
        let base = facts.base_len();
        assert_eq!(
            facts.decls_in_file("C.cs", "x"),
            [(base, NodeKind::Property)]
        );
    }

    /// `src/m.rs`: `fn setup` (lines 0-4) holds `mod inner { name }` with
    /// `name` of `kind` on line 2, a twin-less `lone` of `kind` on line 1
    /// and `impl Foo { fn new }` with `new` on line 3, then the
    /// module-level `mod inner { name }` on line `twin_line`. A `mod` breaks
    /// the lexical link, so both `name`s are unscoped, owned by `inner`, and
    /// share a base uid. Pass 1 stores the second as Pass 1's `UidSlot`
    /// says: a merged repeat (`merges_repeats`) as a tombstone with the uid
    /// `tombstone(overload(base, 1))` and no name, any other repeat as a
    /// later node of its own under `overload(base, 1)`. Returns the bytes
    /// and the idxs of the first, of `new`, of `lone` and of the twin.
    fn inner_twins(kind: NodeKind, name: &str, twin_line: u32) -> (Vec<u8>, u32, u32, u32, u32) {
        const M: &str = "src/m.rs";
        let mut fx = GraphFixture::new();
        let setup = fx.func(M, "setup");
        fx.span(setup, (0, 0, 4, 1));
        let survivor = fx.node_owned(kind, M, "inner", name);
        fx.span(survivor, (2, 8, 2, 30));
        let new = fx.method(M, "Foo", "new");
        fx.span(new, (3, 8, 3, 30));
        let lone = fx.node_owned(kind, M, "inner", "lone");
        fx.span(lone, (1, 8, 1, 30));
        let twin = fx.node_owned(kind, M, "inner", name);
        fx.span(twin, (twin_line, 4, twin_line, 26));
        let assembly = fx.assembly_mut();
        let node = &mut assembly.nodes[twin as usize];
        node.uid = crate::uid::overload(node.uid, 1);
        if crate::uid::merges_repeats(kind) {
            node.uid = crate::uid::tombstone(node.uid);
            node.name = Default::default();
            node.owner_class = Default::default();
        } else {
            let later = &mut assembly.later_nodes;
            later.resize(twin as usize / 8 + 1, 0);
            later[twin as usize / 8] |= 1 << (twin % 8);
        }
        (fx.into_bytes(), survivor, new, lone, twin)
    }

    /// Stands for: the clean side's per-uid AND covers the raws Pass 1
    /// merged, as the index's `rust_body_local_rows` does. A merged
    /// constant's tombstone stores `tombstone(overload(base, n))`; with it
    /// at module level the first is a module item, with it in the body the
    /// first stays body-local. The answer is memoized per node: a twin-less
    /// `lone` asked in between keeps its own answer. A method has no merged
    /// twin and stays body-local. Red before the fix: the scan looked for
    /// `tombstone(base)`, which no node stores since every merged repeat
    /// takes its ordinal's uid, so the first read body-local on both lines.
    #[test]
    fn test_is_body_local_merged_const_twin_decides_first_method_stays_body_local() {
        let removed = FxHashSet::default();
        for (twin_line, want) in [(6, false), (2, true)] {
            let (bytes, survivor, new, lone, _) = inner_twins(NodeKind::Const, "LIMIT", twin_line);
            let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
            let modules = RustModules::new(graph, &[], &removed, Default::default());
            assert_eq!(
                modules.is_body_local(survivor),
                want,
                "twin on line {twin_line}"
            );
            assert!(modules.is_body_local(lone), "a twin-less const in the body");
            assert_eq!(
                modules.is_body_local(survivor),
                want,
                "the memoized answer, twin on line {twin_line}"
            );
            assert!(modules.is_body_local(new), "a method inside the body");
        }
    }

    /// Stands for: an unscoped `fn` repeat is a later node of its own, not
    /// a raw Pass 1 merged into the first, so a module-level repeat leaves
    /// the first in the body body-local. The repeat's own answer comes
    /// from its own span.
    #[test]
    fn test_is_body_local_later_fn_twin_first_stays_body_local() {
        let removed = FxHashSet::default();
        for twin_line in [6, 2] {
            let (bytes, survivor, _, _, twin) =
                inner_twins(NodeKind::Function, "helper", twin_line);
            let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
            assert!(graph.is_later_overload(twin), "the fixture's twin");
            let modules = RustModules::new(graph, &[], &removed, Default::default());
            assert!(modules.is_body_local(survivor), "twin on line {twin_line}");
            assert_eq!(modules.is_body_local(twin), twin_line == 2);
        }
    }

    /// A graph of `files`, each `(file, name)` of `declared` a base `mod`.
    fn manifest_less_graph(files: &[&str], declared: &[(&str, &str)]) -> Vec<u8> {
        let mut fx = GraphFixture::new();
        for file in files {
            fx.file(file);
        }
        for (file, name) in declared {
            fx.node(NodeKind::Module, file, name);
        }
        fx.into_bytes()
    }

    fn dirty_module_file(path: &str, declared: &[&str]) -> OverlayFileInput {
        let symbol = |name: &str| OverlaySymbol {
            is_exported: false,
            name: name.to_string(),
            kind: NodeKind::Module,
            owner_class: None,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
            calls: Vec::new(),
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
            decorators: Vec::new(),
            params: Vec::new(),
            type_annotation: None,
            type_params: Vec::new(),
        };
        OverlayFileInput {
            parsed_language: None,
            rel_path: path.to_string(),
            symbols: declared.iter().map(|name| symbol(name)).collect(),
            imports: Vec::new(),
            export_aliases: Vec::new(),
        }
    }

    /// The text of each file as its `mod` declarations: a loose tree reads
    /// its children from the source, as the index does.
    fn mod_sources(declared: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let declared: Vec<(String, String)> = declared
            .iter()
            .map(|&(file, name)| (file.to_string(), name.to_string()))
            .collect();
        move |file: &str| {
            Some(
                declared
                    .iter()
                    .filter(|(of, _)| of == file)
                    .map(|(_, name)| format!("mod {name};\n"))
                    .collect(),
            )
        }
    }

    /// Where `path` (`self::a::b`) lands from `caller`, through the position
    /// [`RustModules::caller`] reads.
    fn module_from<'a>(modules: &RustModules<'a>, caller: &str, path: &str) -> Option<&'a str> {
        let position = modules.caller(caller);
        let tree = OverlayTree {
            modules,
            caller,
            position: &position,
        };
        ladder::module_tree_module(&tree, caller, path)
    }

    /// Stands for: with no Cargo scope the caller roots a tree of its own
    /// (the index's `loose_scope`): `crate::` names nothing, and an absent
    /// file, a non-Rust file, a path outside the repo and an empty file list
    /// give no position at all.
    #[test]
    fn test_rust_modules_caller_without_cargo_scope_roots_tree_at_caller() {
        let bytes = manifest_less_graph(&["src/lib.rs", "README.md"], &[]);
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let removed = FxHashSet::default();
        let modules = RustModules::new(graph, &[], &removed, Default::default());
        let position = modules.caller("src/lib.rs");
        assert_eq!(position.module, Some(("src/lib.rs", Vec::new())));
        assert_eq!(position.krate, None);
        assert!(position.targets.is_empty());
        for absent in ["src/gone.rs", "README.md", "../outside.rs", ""] {
            assert!(modules.caller(absent).module.is_none(), "{absent:?}");
        }
        let empty = manifest_less_graph(&[], &[]);
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&empty).unwrap();
        let modules = RustModules::new(graph, &[], &removed, Default::default());
        assert!(modules.caller("src/lib.rs").module.is_none());
    }

    /// Stands for: `mod a;` in `lib.rs` places `a.rs` before `a/mod.rs`
    /// beside it, an ordinary file keeps its children in `dir/stem/`, a
    /// module no `mod` declares is no proof whatever file exists, `crate::`
    /// binds nothing, and two lookups of one path agree.
    #[test]
    fn test_rust_modules_manifest_less_tree_places_declared_modules_only() {
        let bytes = manifest_less_graph(
            &[
                "src/lib.rs",
                "src/a.rs",
                "src/a/mod.rs",
                "src/a/b.rs",
                "src/x.rs",
                "src/x/c.rs",
                "src/c.rs",
                "src/stray.rs",
            ],
            &[("src/lib.rs", "a"), ("src/a.rs", "b"), ("src/x.rs", "c")],
        );
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let removed = FxHashSet::default();
        let source = mod_sources(&[("src/lib.rs", "a"), ("src/a.rs", "b"), ("src/x.rs", "c")]);
        let modules =
            RustModules::new(graph, &[], &removed, Default::default()).with_sources(Some(&source));
        for _ in 0..2 {
            assert_eq!(
                module_from(&modules, "src/lib.rs", "self::a"),
                Some("src/a.rs")
            );
        }
        assert_eq!(
            module_from(&modules, "src/lib.rs", "self::a::b"),
            Some("src/a/b.rs")
        );
        assert_eq!(
            module_from(&modules, "src/x.rs", "self::c"),
            Some("src/x/c.rs")
        );
        for unplaced in ["self::stray", "crate::a", "super::a"] {
            assert_eq!(module_from(&modules, "src/lib.rs", unplaced), None);
        }
        assert!(matches!(
            modules.published("src/lib.rs", "src/a.rs", "b"),
            ladder::Published::Own
        ));
    }

    /// Stands for: a dirty file's own `mod` declarations replace the base
    /// ones. Deleting `mod a;` unplaces `a`, adding `mod b;` beside a new
    /// dirty `b.rs` places it, and a dirty `mod.rs` file (`a/mod.rs`) is
    /// still the module's file.
    #[test]
    fn test_rust_modules_manifest_less_tree_follows_dirty_mod_declarations() {
        let bytes = manifest_less_graph(&["src/lib.rs", "src/a/mod.rs"], &[("src/lib.rs", "a")]);
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let removed = FxHashSet::default();
        let base = graph.nodes.len() as u32;
        let facts = |starts: Vec<u32>| DirtyRustFacts {
            starts,
            ..Default::default()
        };

        let deleted = [dirty_module_file("src/lib.rs", &[])];
        let source = mod_sources(&[]);
        let modules = RustModules::new(graph, &deleted, &removed, facts(vec![base]))
            .with_sources(Some(&source));
        assert_eq!(module_from(&modules, "src/lib.rs", "self::a"), None);

        let added = [
            dirty_module_file("src/lib.rs", &["a", "b"]),
            dirty_module_file("src/b.rs", &[]),
        ];
        let source = mod_sources(&[("src/lib.rs", "a"), ("src/lib.rs", "b")]);
        let modules = RustModules::new(graph, &added, &removed, facts(vec![base, base + 2]))
            .with_sources(Some(&source));
        assert_eq!(
            module_from(&modules, "src/lib.rs", "self::b"),
            Some("src/b.rs")
        );
        assert_eq!(
            module_from(&modules, "src/lib.rs", "self::a"),
            Some("src/a/mod.rs")
        );
        assert_eq!(
            modules.caller("src/b.rs").module,
            Some(("src/b.rs", Vec::new()))
        );
    }
}
