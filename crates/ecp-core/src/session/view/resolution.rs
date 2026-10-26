//! Call resolution for the overlay view: the name tables, `OverlayFacts`,
//! the tier helpers and the edge pass of [`super::OverlayView::build_with_removed`].
//! The view owns assembly, the cache and incremental replay; this module owns
//! how a dirty file's references bind.

use super::{
    base_function_meta, construction_target, emit_closure_references, fragment_emits, reason_index,
    span_of, CallGap, DirtyNamesake, OverlayFileInput, OverlaySymbol, ViewEdge, ViewNode,
    CONF_IMPORT_SCOPED, CS_STRICTEST_FLAGS, GAP_CS_ACCESS_DIRTY_TARGET,
    GAP_CS_EXTENSION_DIRTY_DECLARATION, GAP_CS_PARTIAL_BARE_CALL,
};
use crate::analyzer::csharp_access::{self, CallAccess};
use crate::analyzer::csharp_extension::{self, ReceiverSite};
use crate::analyzer::heritage::{enclosing_class_heritage, holds_heritage, HeritageNode};
use crate::analyzer::import_binding::{
    fqn_imported_name, fqn_language, fqn_type_path, import_binding, import_member_fallback,
    owner_path_agrees, DeclaredType, MemberOwner,
};
pub(super) use crate::analyzer::ladder::qualifier_prefix_is_internal;
use crate::analyzer::ladder::{
    self, split_qualifier, CallContext, CallPick, CrateModule, FieldRead, ResolveFacts, StemFile,
    TypeName,
};
use crate::analyzer::lexical::{
    lexical_chain_end, lexical_scope_kinds, twin_enclosing, LexicalFunctions,
};
use crate::analyzer::outcome::{Resolution, Tier};
use crate::analyzer::path_aliases::PathAliases;
use crate::analyzer::reexport::{
    self, export_filter_applies, is_ecmascript, ExportAliasTable, Fact, FileImports, ImportRef,
    ModuleFacts, PyModule, ResolveTarget,
};
use crate::analyzer::rust_paths::{
    is_rust_source, rust_expand_path_head, rust_module_dir, rust_module_path_base,
    rust_type_path_prefix,
};
use crate::analyzer::same_file_pick::{
    is_free, prefer_owned, rust_impl_members, ImplMember, ImplNode, PickFacts, PickScope,
};
use crate::analyzer::type_ladder::{
    self, TypeCandidates, TypeFacts, TypeMember as LadderMember, MAX_HERITAGE_DEPTH,
};
use crate::analyzer::types::{
    owner_key, CallArguments, CallSite, NameRef, RawImport, Span, VariantRef,
    FUNCTION_REFERENCE_REASON, NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON, VALUE_READ_REASON,
    VARIANT_PATTERN_REASON, VARIANT_USE_REASON,
};
use crate::contracts::identity::lexical_parents;
use crate::file_category::{pick_global, FileMeta, GlobalPick, Language};
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use crate::session::import_scope::{ImportScope, Module};
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

/// Overlay symbols of one kind family (`kind`), by name: the Tier-3
/// overlay-wide candidates of [`resolve_callee`], and the names each dirty
/// file declares.
struct OverlayNames<'a> {
    /// The kind family, as the index's lookups name it.
    target: ResolveTarget,
    /// (file ordinal, name) the file declares. Tier 1 picks through
    /// [`OverlayFacts::decls_in_file`]; only the qualifier tier asks here.
    same_file: FxHashSet<(usize, &'a str)>,
    /// name → (virtual idx, file meta), anywhere in the overlay.
    anywhere: FxHashMap<&'a str, Vec<(u32, FileMeta)>>,
}

impl<'a> OverlayNames<'a> {
    fn new(target: ResolveTarget) -> Self {
        Self {
            target,
            same_file: FxHashSet::default(),
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
        self.same_file.insert((file_ord, node.name.as_str()));
        if first_of_uid {
            self.anywhere
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
/// `super()`, a Rust caller, whose `impl` block the same-file pick prefers,
/// and whether the Rust module tree runs).
type QualifiedCache = FxHashMap<
    (
        usize,
        (usize, usize),
        Box<str>,
        ResolveTarget,
        Option<(usize, usize)>,
        Option<u32>,
        bool,
    ),
    Resolution,
>;
type NamedImportCache = FxHashMap<(usize, ResolveTarget), FxHashMap<Box<str>, Option<u32>>>;
/// Every input of [`resolve_callee`]'s Tier 3 that varies within one view:
/// (dirty file ordinal, its imports slice, callee, kind family, whether a
/// call site admits the nearest copy, and Rust method syntax).
type GlobalCache = FxHashMap<
    (
        usize,
        (usize, usize),
        Box<str>,
        ResolveTarget,
        bool,
        bool,
        bool,
    ),
    Option<(u32, f32)>,
>;

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
    /// File → its imports as [`RawImport`]s, built on first read: a file's
    /// chained calls type their receivers through one callee file's imports.
    raw_imports: RefCell<FxHashMap<String, Rc<[RawImport]>>>,
    /// Module → its resolved `export *` modules, as the index resolves them
    /// once per module.
    star_modules: RefCell<StarModuleCache>,
    /// (dirty file ordinal, kind family) → callee → the named-import tier's
    /// answer: a file calls one imported name at many sites, and each probe
    /// walks the specifier's candidate files.
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
    /// The path aliases the index resolved with (tsconfig / jsconfig
    /// `paths`, go.mod, Cargo.toml, ...), read once per view.
    aliases: PathAliases,
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
    /// The Rust module trees ([`ladder::ModuleTreeFacts`]).
    rust: RustModules<'a>,
    /// Dirty file ordinal → where it stands in [`Self::rust`], read on its
    /// first module-tree question.
    rust_callers: RefCell<FxHashMap<usize, Rc<RustCaller<'a>>>>,
}

/// A base Rust `Impl` node: its span, its idx and the [`owner_key`] of the
/// trait it implements.
/// A type's resolved bases in declared order, and its first unresolved one.
type Supertypes = (Vec<u32>, Option<u32>);
type ImplSpan<'a> = ((u32, u32, u32, u32), u32, Option<&'a str>);

impl<'a> OverlayFacts<'a> {
    fn new(
        graph: &'a ArchivedZeroCopyGraph,
        files: &'a [OverlayFileInput],
        nodes: &'a [ViewNode],
        dirty_base: &'a FxHashSet<u32>,
        removed: &'a FxHashSet<&'a str>,
        replaced: &'a FxHashMap<u32, u32>,
        later_overloads: &'a FxHashMap<u32, Vec<u32>>,
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
        let mut seen_uids: FxHashSet<u64> = FxHashSet::default();
        let mut offset = 0usize;
        for (file_ord, file) in files.iter().enumerate() {
            let mut exports_any = false;
            let first = offset;
            for symbol in &file.symbols {
                let node = &nodes[offset];
                if !node.later_overload && seen_uids.insert(node.uid) {
                    let virt = base_len + offset as u32;
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
                exported.push(symbol.is_exported);
                if let Some(return_type) = symbol.return_type.as_deref() {
                    dirty_return_types.insert(base_len + offset as u32, return_type);
                }
                if let Some(flags) = symbol.flags {
                    dirty_flags.insert(base_len + offset as u32, flags);
                }
                offset += 1;
            }
            if is_rust_source(std::path::Path::new(&file.rel_path)) {
                let start = base_len + first as u32;
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
            if !scoped.is_empty() {
                let start = base_len + first as u32;
                // The fields `file_identities` numbers the file from.
                let decls: Vec<(NodeKind, Span)> = file
                    .symbols
                    .iter()
                    .map(|s| (s.kind, HeritageNode::span(s)))
                    .collect();
                for (offset, parent) in lexical_parents(&decls, scoped).into_iter().enumerate() {
                    if let Some(parent) = parent {
                        dirty_lexical_parents.insert(start + offset as u32, start + parent);
                    }
                }
            }
            dirty_namespaces.push(namespace_owners(&file.symbols));
            let aliases = ExportAliasTable::build(alias_pairs(file));
            es_modules.push(reexport::is_es_module(
                Language::from_normalized_path(&file.rel_path),
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
            let rust_file = graph
                .files
                .get(block.file_idx.to_native() as usize)
                .is_some_and(|f| {
                    Language::from_normalized_path(f.path.resolve(pool)) == Language::Rust
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
        Self {
            graph,
            scope: ImportScope::new(graph, files, dirty_base, removed),
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
            raw_imports: RefCell::default(),
            star_modules: RefCell::default(),
            named_imports: RefCell::default(),
            qualified: RefCell::default(),
            global: RefCell::default(),
            stems: OnceCell::new(),
            aliases: graph.path_aliases(),
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
            csharp_namespaces: OnceCell::new(),
            rust: RustModules::new(graph, files, removed),
            rust_callers: RefCell::default(),
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
        let base = base.to_string_lossy().replace('\\', "/");
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
                ladder::module_tree_item(tree, source, &path)
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
                .same_namespaces(Language::from_normalized_path(a_file), a_file, b_file)
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
    /// tree lookup of the callee, `None` where the caller stops a Rust path
    /// at the qualifier tier ([`RustTail::Stop`]).
    fn qualified(
        &self,
        file_ord: usize,
        site: ladder::QualifiedSite<'_>,
        qualifiers: &OverlayNames<'_>,
        heritage: &[String],
        module_tree: Option<impl FnOnce() -> Option<u32>>,
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
            module_tree.is_some(),
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
            || module_tree.and_then(|lookup| lookup()),
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
        if !export_filter_applies(file) {
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
        // `for_each_specifier_candidate` with its path aliases.
        if let Some(file) = self.anchored_rust_module(importer, specifier) {
            visit(file);
            return;
        }
        let aliases = &self.aliases;
        reexport::for_each_specifier_candidate(
            std::path::Path::new(importer),
            specifier,
            |expanded| {
                if !aliases.is_empty() {
                    aliases.expand(specifier, |alias| expanded(alias));
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

    /// The [`FileMeta`] of the file `idx` is declared in.
    fn meta_of(&self, idx: u32) -> FileMeta {
        self.node_file(idx)
            .map_or_else(FileMeta::default, FileMeta::from_normalized_path)
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
        let graph = self.graph;
        let node = &graph.nodes[first as usize];
        let owner = node.owner_class.resolve(&graph.string_pool);
        let mut later: Vec<u32> = graph
            .nodes_by_name(node.name.resolve(&graph.string_pool))
            .filter(|&idx| {
                let other = &graph.nodes[idx as usize];
                idx != first
                    && other.file_idx == node.file_idx
                    && NodeKind::from(&other.kind) == NodeKind::from(&node.kind)
                    && other.owner_class.resolve(&graph.string_pool) == owner
                    && graph.is_later_overload(idx)
            })
            .collect();
        later.sort_unstable();
        later
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
        let impls = self.base_impls.get_or_init(|| {
            let mut impls: FxHashMap<u32, Vec<ImplSpan<'a>>> = FxHashMap::default();
            for impl_idx in graph.nodes_by_kind(NodeKind::Impl) {
                let block = &graph.nodes[impl_idx as usize];
                let rust = graph
                    .files
                    .get(block.file_idx.to_native() as usize)
                    .is_some_and(|f| {
                        is_rust_source(std::path::Path::new(f.path.resolve(&graph.string_pool)))
                    });
                if !rust || self.dirty_base.contains(&impl_idx) {
                    continue;
                }
                let span = (
                    block.span.0.to_native(),
                    block.span.1.to_native(),
                    block.span.2.to_native(),
                    block.span.3.to_native(),
                );
                impls.entry(block.file_idx.to_native()).or_default().push((
                    span,
                    impl_idx,
                    owner_key(block.owner_class.resolve(&graph.string_pool)),
                ));
            }
            impls
        });
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
        let stale = self
            .node_file(ty)
            .filter(|&file| Language::from_normalized_path(file) == Language::Rust)
            .and_then(|_| owner_key(self.name_of(ty)))
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
        let csharp = self
            .node_file(id)
            .is_some_and(|file| Language::from_normalized_path(file) == Language::CSharp);
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
        if Language::from_normalized_path(file) != Language::CSharp
            || split_qualifier(name).is_some()
            || !matches!(target, ResolveTarget::Callable | ResolveTarget::Method)
        {
            return None;
        }
        let owner = owner_key(owner)?;
        let ty = self.first_in_file(file, owner, ResolveTarget::Type, None)?;
        Some((ty, owner))
    }

    /// The index's `Resolver::partial_type_member`: the member a bare C#
    /// call `name()` reaches in its caller's own type `owner` when that type
    /// has `partial` parts ([`Self::partial_group`]). Only a member the type
    /// owns in any part (0.85) or inherits (0.8) binds; `None` for any other
    /// call or verdict.
    fn partial_type_member(
        &self,
        file: &str,
        name: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> Option<(u32, f32)> {
        let (ty, owner) = self.partial_caller_type(file, name, owner, target)?;
        if self.partial_group(ty).is_empty() {
            return None;
        }
        owned_or_inherited(type_ladder::member_via_type(
            self,
            TypeCandidates::Unique(ty, owner),
            file,
            name,
            target,
            true,
        ))
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
                        other_file != file
                            && Language::from_normalized_path(other_file) == Language::CSharp
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
        let language = Language::from_normalized_path(source);
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
                if !self.has_file(candidate) {
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
        let caller = FileMeta::from_normalized_path(source);
        if caller.language == Language::Rust && !imported_in_project {
            return TypeCandidates::None;
        }
        let ids = TypeFacts::global_candidates(self, qualifier, ResolveTarget::Type, caller);
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
            FileMeta::from_normalized_path(source),
        );
        ids.retain(|&id| {
            self.node_file(id)
                .is_some_and(|file| self.owner_path_agrees(file, path))
        });
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

    /// The index's `Resolver::resolve_chained_member`: the member `member`
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
        let language = Language::from_normalized_path(callee_file);
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
                Language::CSharp => type_ladder::visible_return_type(self, callee_file, ty, |id| {
                    self.node_file(id).is_some_and(|file| {
                        self.scope.namespace_return_type_visible(
                            file,
                            self.owner_of(id),
                            callee_file,
                            &imports,
                        )
                    })
                }),
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
    /// edge only: the overlay records no BlindSpot. The overlay reads only
    /// the clean files' extension declarations. When a dirty C# file declares
    /// a method named `member` that is an extension, or whose flags the
    /// fragment lacks, no edge binds and the result is `Err(DirtyNamesake)`:
    /// the caller records a [`CallGap`], so the payload says the overlay fell
    /// back.
    fn csharp_extension_edge(
        &self,
        source: &str,
        receiver: ReceiverSite<'_>,
        member: &str,
        arguments: Option<CallArguments<'_>>,
        imports: &[RawImport],
        owner: Option<&str>,
    ) -> Result<Option<(u32, f32)>, DirtyNamesake> {
        let dirty_namesake = self.dirty_by_name.get(member).is_some_and(|ids| {
            ids.iter().any(|&id| {
                self.kind_of(id) == NodeKind::Method
                    && self.node_file(id).is_some_and(|file| {
                        Language::from_normalized_path(file) == Language::CSharp
                    })
                    && (!self.csharp_flags_known(id)
                        || self.csharp_method_flags(id) & csharp_extension::EXTENSION_FLAG != 0)
            })
        });
        if dirty_namesake {
            return Err(DirtyNamesake);
        }
        let graph = self.graph;
        let pool = &graph.string_pool;
        let mut decls: Vec<csharp_extension::ExtensionDecl<'_>> = Vec::new();
        for id in graph.nodes_by_name(member) {
            if self.dirty_base.contains(&id) || self.name_of(id) != member {
                continue;
            }
            let Some(meta) = base_function_meta(graph, id) else {
                continue;
            };
            let flags = meta.flags.to_native();
            if flags & csharp_extension::EXTENSION_FLAG == 0 {
                continue;
            }
            let (Some(file), Some(class)) = (self.node_file(id), self.owner_of(id)) else {
                continue;
            };
            let namespace = self
                .first_in_file(file, class, ResolveTarget::Type, None)
                .and_then(|class_id| {
                    type_ladder::declared_namespace(
                        self.owner_of(class_id),
                        self.csharp_namespaces(file).iter().copied(),
                    )
                });
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
                namespace,
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
                FileMeta::from_normalized_path(scope_file),
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
                    if Language::from_normalized_path(path) == Language::CSharp {
                        note(path, node.name.resolve(pool));
                    }
                }
                for file in self.scope.dirty() {
                    if Language::from_normalized_path(&file.rel_path) != Language::CSharp {
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
        if Language::from_normalized_path(file) == Language::Go {
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
        let scope_meta = FileMeta::from_normalized_path(scope_file);
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

    /// A dirty function's from its fresh parse; a clean one's from the
    /// archived `FunctionMeta`, which the index wrote from the same
    /// `RawFunctionMeta.return_type`. A base node of a dirty file is stale.
    fn return_type_of(&self, callee: u32) -> Option<&str> {
        if callee >= self.base_len() {
            return self.dirty_return_types.get(&callee).copied();
        }
        if self.dirty_base.contains(&callee) {
            return None;
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

/// The index's `resolve_name_ref` in `builder.rs` ([`ladder::name_ref`]):
/// `resolve` is [`resolve_callee`] over the name table of the target's
/// kinds, and the import tier alone is [`ladder::imported_member`] over the
/// fields.
#[allow(clippy::too_many_arguments)]
fn resolve_overlay_name_ref(
    graph: &ArchivedZeroCopyGraph,
    file_ord: usize,
    file: &OverlayFileInput,
    caller: FileMeta,
    owner: Option<&str>,
    name_ref: NameRef<'_>,
    lexical: Option<u32>,
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
) -> Option<(u32, f32)> {
    ladder::name_ref(
        facts,
        &file.rel_path,
        caller.language,
        &file.imports,
        owner,
        name_ref,
        lexical,
        |name, target| {
            let names = match target {
                ResolveTarget::Callable => callables,
                ResolveTarget::Value => values,
                _ => return Vec::new(),
            };
            resolve_callee(
                graph,
                CallSite::Plain(name),
                file_ord,
                &file.imports,
                caller,
                base_metas,
                names,
                types,
                qualifiers,
                nodes,
                replaced,
                dirty_base,
                facts,
                Scope {
                    call: None,
                    heritage: &[],
                    type_lookup: false,
                    rust_tail: RustTail::Stop,
                },
            )
            .into_iter()
            .collect()
        },
        // The name tiers ask the import tier alone for a field only. The
        // explicit imports only: the index's `resolve_imported_symbol` also
        // takes the wildcard pass (divergence, `ladder_import_member_parity`).
        |name, _| match ladder::imported_member(
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
            ladder::MemberHit::Bound(hit) => vec![(hit.id, CONF_IMPORT_SCOPED)],
            ladder::MemberHit::Unbound { .. } => Vec::new(),
        },
    )
}

/// Mirror of index-time Pass-2 `Calls` resolution, narrowed to the inputs
/// available at query time, over the kind family of `names`. Returns the
/// merged-space target index.
/// Rust `::` calls use the cached module-path branch in `resolve_edges` first.
/// Method eligibility is a post-filter, like index-time `lookup_call_in_file`:
/// removing free functions before uniqueness checks would invent a winner.
///
/// A Python from-imported submodule and a Go package selector are settled
/// by the call closure in `resolve_edges` before this runs, as the index's
/// `Resolver::resolve_call` settles them.
/// Tier 1 — same file: the dirty file was FULLY re-parsed, so its own
/// callable set is authoritative. The first match ([`reexport::pick_call`]),
/// narrowed at a call site by [`prefer_owned`] → confidence 1.0; several
/// unrelated owners bind nothing here and fall through.
/// Tier 1.5 — a bare C# call in a part of a `partial` type binds the
/// member another part declares or inherits
/// ([`OverlayFacts::partial_type_member`]) → 0.85 or 0.8.
/// Tier 2 — import-scoped. In an [`import_member_fallback`] language,
/// [`ladder::import_member`] is the index's import-member tier, a Python package
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
/// takes the member in the qualifier's file through the shared
/// [`ladder::qualified`] ([`OverlayFacts::qualified`]) → 0.85, else the
/// receiver-typing ladder ([`OverlayFacts::receiver_ladder`]), else no edge;
/// it never reaches Tier 3. A Rust path the qualifier tier leaves unresolved
/// goes on to the module tree (the ladder decides that position) and then
/// the receiver ladder, at call sites and type lookups only
/// ([`RustTail`]).
/// Tier 2.75 — heritage: a bare name in a class with declared bases takes
/// the member in the first base's file that declares it → 0.8. Then, in a
/// fully qualified language, the wildcard imports bind.
/// Tier 3 — global: all clean-base callables (via the archived `name_index`)
/// plus all overlay callables, through the index-time candidate filter
/// [`pick_global`] (language and vendor barriers, unique only). ≥2
/// remaining → suppressed, matching `DecisionTier::AmbiguousGlobal` (an
/// invented edge is worse than a missing one), unless `call` is given and
/// every candidate is a copy of one declaration: then the copy nearest the
/// caller binds ([`ladder::nearest_copy`]) at
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
    scope: Scope<'_>,
) -> Option<(u32, f32)> {
    let importer = facts.scope.dirty()[file_ord].rel_path.as_str();
    let callee = site.name();
    let call = scope.call;
    let rust_method = site.uses_method_syntax() && caller.language == Language::Rust;
    let accepts = |idx: u32| !rust_method || is_method_node(graph, nodes, idx);
    // Tier 1: same-file, the index's `lookup_call_in_file`. At a call site
    // the pick narrows by owner (`prefer_owned`); when the file holds
    // several unrelated owners of the name it binds nothing and the later
    // tiers decide.
    let call_target = if rust_method {
        ResolveTarget::Method
    } else {
        names.target
    };
    if let Resolution::Bound(hit) =
        ladder::same_file(facts, importer, callee, call_target, call, scope.heritage)
    {
        return accepts(hit.id).then_some((hit.id, hit.confidence));
    }

    // The index's Tier 1 across the files of a C# partial type: a bare call
    // in one part reaches what the caller's type owns in any part, or
    // inherits (codex ruling P-d).
    if let Some(hit) = call
        .filter(|call| !call.untyped && !call.free_only)
        .and_then(|call| facts.partial_type_member(importer, callee, call.owner?, call_target))
    {
        return Some(hit);
    }

    // The index binds no import for an untyped member call, and in these
    // languages has no path-segment import tier either. A type lookup in a
    // fully qualified language takes the named-import tier instead.
    let member_policy = import_member_fallback(caller.language)
        && !(scope.type_lookup && fqn_language(caller.language));
    let binds_imports = !imports.is_empty()
        && !matches!(site, CallSite::UntypedMember(_))
        && names.target != ResolveTarget::Field;
    // Set when no explicit import binds the callee: the wildcard imports
    // bind after the heritage tier.
    let mut wildcard_open = false;
    if member_policy && binds_imports {
        match ladder::import_member(
            &OverlayImports {
                facts,
                names,
                types,
            },
            importer,
            caller.language,
            callee,
            imports,
            names.target,
            scope.heritage,
        ) {
            ladder::ImportMember::Bound(hit) => return Some((hit.id, CONF_IMPORT_SCOPED)),
            ladder::ImportMember::External => return None,
            ladder::ImportMember::Retry(name) => {
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
                    scope,
                );
            }
            ladder::ImportMember::Continue { wildcard } => wildcard_open = wildcard,
            ladder::ImportMember::Extension(member) => {
                return extension_fallback(
                    graph,
                    member,
                    file_ord,
                    imports,
                    caller,
                    base_metas,
                    (names, types, qualifiers),
                    nodes,
                    replaced,
                    dirty_base,
                    facts,
                    scope.heritage,
                );
            }
        }
    }
    let ecmascript = is_ecmascript(caller.language);
    if ecmascript && binds_imports {
        if let Some(decl) = facts.named_import(file_ord, imports, callee, names.target) {
            debug_assert!(!replaced.contains_key(&decl));
            return accepts(decl).then_some((decl, CONF_IMPORT_SCOPED));
        }
    }

    // Built on first use: most calls bind at the qualifier, ladder or
    // heritage tier, which never read the bare name's candidates.
    let base_candidates = OnceCell::new();
    let base = || {
        base_candidates
            .get_or_init(|| clean_base(graph, names.target, dirty_base, callee))
            .as_slice()
    };
    let overlay_candidates: &[(u32, FileMeta)] =
        names.anywhere.get(callee).map(Vec::as_slice).unwrap_or(&[]);

    // Tier 2: import-scoped, the index's named-import tier: every import
    // binding the callee, in order, until one's module declares the name.
    let segment_imports: &[RawImport] = if member_policy || names.target == ResolveTarget::Field {
        &[]
    } else {
        imports
    };
    let base_len = graph.nodes.len() as u32;
    for (import, declared) in segment_imports
        .iter()
        // A TS / JS relative or path-aliased import was probed exactly
        // above; a package specifier keeps the segment approximation.
        // TS / JS takes the index's named-import tier above, which has no
        // segment match.
        .filter(|_| !ecmascript)
        .filter_map(|i| Some((i, import_binding(i, callee, caller.language)?.name)))
    {
        let base_declared;
        let (base_scoped, overlay_scoped): (&[u32], &[(u32, FileMeta)]) = if declared == callee {
            (base(), overlay_candidates)
        } else {
            base_declared = clean_base(graph, names.target, dirty_base, declared);
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
        let scoped = base_scoped
            .iter()
            .map(|&idx| (idx, base_path(graph, idx)))
            .chain(
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
                    Some((
                        relative_module_rank(importer, &import.source, path?, &facts.aliases)?,
                        idx,
                    ))
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

    // Tier 2.5, then Tier 3.5 for a Rust `::` path ([`ladder::qualified`]):
    // a qualified callee resolves through its qualifier or not at all; the
    // index never retries the bare member (`std::fs::read` must not bind a
    // project `read`).
    if let Some((qualifier, member)) = split_qualifier(callee) {
        let target = match names.target {
            ResolveTarget::Callable
                if caller.language == Language::Rust && site.uses_method_syntax() =>
            {
                ResolveTarget::Method
            }
            target => target,
        };
        let rust_path = caller.language == Language::Rust && callee.contains("::");
        let ladder_tail = matches!(scope.rust_tail, RustTail::Ladder);
        let module_tree = (ladder_tail && rust_path).then_some(move || {
            facts.with_rust_tree(file_ord, |tree| {
                ladder::module_tree_member(tree, facts, importer, callee, member, target)
            })
        });
        let site = ladder::QualifiedSite {
            source: importer,
            caller,
            language: caller.language,
            imports,
            callee,
            qualifier,
            member,
            target,
            call,
        };
        match facts.qualified(file_ord, site, qualifiers, scope.heritage, module_tree) {
            Resolution::Bound(hit) => return Some((hit.id, hit.confidence)),
            Resolution::NoEdge(_) => return None,
            Resolution::Unknown | Resolution::Shadowed => {}
        }
        // A Rust path the caller stops at the qualifier tier takes no ladder.
        if rust_path && !ladder_tail {
            return None;
        }
        // The receiver-typing ladder: only where every tier above found
        // nothing, so it adds edges and never moves one.
        let verdict = facts.receiver_ladder(
            importer,
            imports,
            (callee, qualifier, member),
            target,
            call.is_some(),
        );
        // A Kotlin receiver named by a class import was the bare member before
        // the import tier typed it, which finds an extension function on the
        // type in another file. A heritage qualifier (`super.x()`) keeps the
        // ladder's verdict.
        let extension = match verdict {
            LadderMember::NoEdge => ladder::kotlin_extension_retry(
                caller.language,
                callee,
                (qualifier, member),
                imports,
                scope.heritage,
            ),
            _ => None,
        };
        if let Some(member) = extension {
            return extension_fallback(
                graph,
                member,
                file_ord,
                imports,
                caller,
                base_metas,
                (names, types, qualifiers),
                nodes,
                replaced,
                dirty_base,
                facts,
                scope.heritage,
            );
        }
        return ladder_edge(verdict);
    }

    // Tier 2.75: the caller's bases, in declared order, each as an implicit
    // qualifier; the first base whose file holds the member decides. A bare
    // call that reaches no member skips it.
    if let (Resolution::Bound(hit), _) =
        ladder::heritage(facts, scope.heritage, callee, names.target, call, |base| {
            ladder::qualifier_file(
                &OverlayQualifiers {
                    facts,
                    file_ord,
                    qualifiers,
                },
                importer,
                caller,
                imports,
                (None, base, callee),
                names.target,
                StemFile::DeclaringMember,
            )
        })
    {
        return accepts(hit.id).then_some((hit.id, hit.confidence));
    }

    if let Some(hit) = ladder::wildcard_member(
        &OverlayImports {
            facts,
            names,
            types,
        },
        wildcard_open,
        importer,
        caller.language,
        callee,
        imports,
        names.target,
    ) {
        return Some((hit.id, CONF_IMPORT_SCOPED));
    }

    // Tier 3 ([`global_tier`]).
    global_tier(
        graph,
        callee,
        file_ord,
        imports,
        caller,
        base_metas,
        names,
        nodes,
        replaced,
        dirty_base,
        facts,
        (call, rust_method),
        &base_candidates,
    )
}

/// R17 over the overlay, the index's `Resolver::extension_fallback`
/// ([`ladder::extension_fallback`]): `member` through [`resolve_callee`]
/// outside a call scope, with the caller's `heritage`, top-level functions
/// only.
#[allow(clippy::too_many_arguments)]
fn extension_fallback(
    graph: &ArchivedZeroCopyGraph,
    member: &str,
    file_ord: usize,
    imports: &[RawImport],
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    (names, types, qualifiers): (&OverlayNames<'_>, &OverlayNames<'_>, &OverlayNames<'_>),
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    heritage: &[String],
) -> Option<(u32, f32)> {
    ladder::extension_fallback(facts, member, |member| {
        resolve_callee(
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
            Scope {
                call: None,
                heritage,
                type_lookup: false,
                rust_tail: RustTail::Stop,
            },
        )
    })
    .next()
}

/// Whether the merged-space `idx` is a `Method`.
fn is_method_node(graph: &ArchivedZeroCopyGraph, nodes: &[ViewNode], idx: u32) -> bool {
    match idx.checked_sub(graph.nodes.len() as u32) {
        Some(offset) => nodes[offset as usize].kind == NodeKind::Method,
        None => NodeKind::from(&graph.nodes[idx as usize].kind) == NodeKind::Method,
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

/// Tier 3 of [`resolve_callee`] and of a bare field read
/// ([`ladder::field_read`]): the shared candidate filter
/// ([`ladder::global`]) over the clean base and the overlay declarations
/// named `callee` in the kinds of `names`, memoized per caller file. Same
/// invariant as Tier 2: a base candidate is clean by construction, never
/// redirected; overlay candidates are virtual indices. `call` is the call
/// site, `rust_method` a Rust method-syntax call; `base_candidates` holds
/// the clean base candidates once an earlier tier built them.
#[allow(clippy::too_many_arguments)]
fn global_tier(
    graph: &ArchivedZeroCopyGraph,
    callee: &str,
    file_ord: usize,
    imports: &[RawImport],
    caller: FileMeta,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    names: &OverlayNames<'_>,
    nodes: &[ViewNode],
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    (call, rust_method): (Option<CallPick<'_>>, bool),
    base_candidates: &OnceCell<Vec<u32>>,
) -> Option<(u32, f32)> {
    let importer = facts.scope.dirty()[file_ord].rel_path.as_str();
    // Read from the call site, never from the callee: a retry under the
    // short name (`ladder::ImportMember::Retry`) is no receiverless call.
    let free_only = call.is_some_and(|call| call.free_only);
    let key = (
        file_ord,
        (imports.as_ptr() as usize, imports.len()),
        Box::<str>::from(callee),
        names.target,
        call.is_some(),
        rust_method,
        free_only,
    );
    if let Some(&hit) = facts.global.borrow().get(&key) {
        return hit;
    }
    let call_target = if rust_method {
        ResolveTarget::Method
    } else {
        names.target
    };
    let base_len = graph.nodes.len() as u32;
    // The ladder reads the candidates lazily, so the meta table sits in a
    // cell: a `Fn` stream fills it as the pick advances.
    let base_metas = RefCell::new(base_metas);
    let base_meta = |idx: u32| {
        let file_idx = graph.nodes[idx as usize].file_idx.to_native() as usize;
        *base_metas.borrow_mut().entry(file_idx).or_insert_with(|| {
            graph
                .files
                .get(file_idx)
                .map_or_else(FileMeta::default, |f| {
                    FileMeta::from_path(f.path.resolve(&graph.string_pool))
                })
        })
    };
    // I5 (a scoped node is no global candidate) is the ladder's filter.
    let admits = |&(idx, _): &(u32, FileMeta)| !free_only || !is_method_node(graph, nodes, idx);
    let overlay_candidates: &[(u32, FileMeta)] =
        names.anywhere.get(callee).map(Vec::as_slice).unwrap_or(&[]);
    let clean: &[u32] =
        base_candidates.get_or_init(|| clean_base(graph, names.target, dirty_base, callee));
    // Read up to three times by the ladder (the pick, the nearest copy, the
    // count), and only on a memo miss. The pick stops at the second
    // admitted candidate, so a common name meets few of its base metas.
    let (base_meta, admits) = (&base_meta, &admits);
    let candidates = move || {
        clean
            .iter()
            .map(move |&idx| (idx, base_meta(idx)))
            .chain(overlay_candidates.iter().copied())
            .filter(admits)
    };
    let hidden = |target: u32| {
        let decl_file = match target.checked_sub(base_len) {
            Some(offset) => Some(&*nodes[offset as usize].rel_path),
            None => base_path(graph, target),
        };
        decl_file.is_some_and(|file| {
            reexport::import_hides(
                facts,
                importer,
                callee,
                imports.iter().map(ImportRef::from),
                file,
            )
        })
    };
    // The index's `DecisionTier::NearestCopy` binds for call sites only.
    let hit = ladder::global(
        facts,
        importer,
        caller,
        call_target,
        call.is_some(),
        candidates,
        hidden,
    )
    .edge();
    debug_assert!(hit.is_none_or(|(target, _)| !replaced.contains_key(&target)));
    facts.global.borrow_mut().insert(key, hit);
    hit
}

/// What a lookup carries besides the name, as the index's
/// `resolve_symbol_with_import_binding` arguments.
#[derive(Clone, Copy, Default)]
struct Scope<'c> {
    /// The call site, `None` for a plain symbol lookup. Its presence also
    /// admits the nearest-copy rule of Tier 3. `caller` is a virtual idx.
    call: Option<CallPick<'c>>,
    /// The declared bases of the caller's enclosing class
    /// ([`enclosing_class_heritage`]), probed by the heritage tier.
    heritage: &'c [String],
    /// A type lookup outside a call (`resolve_symbol(.., Type)`): in a fully
    /// qualified language the index binds it through its named-import tier,
    /// not the import-member tier.
    type_lookup: bool,
    /// What a Rust `::` path the qualifier tier leaves unbound takes next.
    rust_tail: RustTail,
}

/// The tiers after the qualifier tier for a Rust `::` path. The index runs
/// the module tree and the receiver-typing ladder for every lookup
/// (`Resolver::resolve_symbol_with_import_binding`); the overlay runs them
/// for a call site and a type lookup only. The rest of its lookups stop at
/// the qualifier tier: a kept divergence, which this parameter makes
/// explicit.
#[derive(Clone, Copy, Default)]
enum RustTail {
    /// No tier after the qualifier tier: a name reference, a qualified field
    /// read, a constructor's type and an extension retry.
    #[default]
    Stop,
    /// The module tree ([`ladder::module_tree_member`]), then the
    /// receiver-typing ladder: a call site and a type lookup, as the index.
    Ladder,
}

/// The overlay's Rust module trees, laid out as the index lays out its own
/// (`RustWorkspaceModTree`): each Cargo target of the crate table
/// `graph.bin` stores (a crate's lib entry, then its bins) roots a tree of
/// file-backed `mod` declarations. Without a stored table (no
/// `Cargo.toml`) there is no tree, as in the index. Dirty declarations
/// replace archived declarations, including deletion. A base path the
/// worktree removed is no file.
pub(super) struct RustModules<'a> {
    graph: &'a ArchivedZeroCopyGraph,
    dirty: FxHashMap<&'a str, &'a OverlayFileInput>,
    removed: &'a FxHashSet<&'a str>,
    /// The `graph.bin` workspace facts, read on first use: reading them
    /// deserializes and validates the whole table, once per view.
    pub(super) crates: OnceCell<crate::contracts::workspace::WorkspaceFacts>,
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
    pub(super) fn new(
        graph: &'a ArchivedZeroCopyGraph,
        dirty: &'a [OverlayFileInput],
        removed: &'a FxHashSet<&'a str>,
    ) -> Self {
        Self {
            graph,
            dirty: dirty
                .iter()
                .map(|file| (file.rel_path.as_str(), file))
                .collect(),
            removed,
            crates: OnceCell::new(),
        }
    }

    fn table(&self) -> &crate::contracts::workspace::WorkspaceFacts {
        self.crates.get_or_init(|| self.graph.workspace_facts())
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
        if self.removed.contains(requested) {
            return None;
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

    /// Whether `file` declares a node of `kind` named `name`; any kind when
    /// `kind` is `None`. A dirty file answers from its fresh parse.
    fn declares(&self, file: &str, name: &str, kind: Option<NodeKind>) -> bool {
        let wanted = |found: NodeKind| kind.is_none_or(|kind| kind == found);
        match self.dirty.get(file) {
            Some(dirty) => dirty
                .symbols
                .iter()
                .any(|s| s.name == name && wanted(s.kind)),
            None => self.graph.nodes_by_name(name).any(|idx| {
                let node = &self.graph.nodes[idx as usize];
                wanted(NodeKind::from(&node.kind))
                    && self.graph.files[node.file_idx.to_native() as usize]
                        .path
                        .resolve(&self.graph.string_pool)
                        == file
            }),
        }
    }

    /// The file of the module `name` that `parent` declares (`mod name;`),
    /// as the index places it: `name.rs`, else `name/mod.rs`, beside a
    /// target root and in a module's own directory otherwise.
    fn child(&self, parent: &'a str, name: &str, parent_is_root: bool) -> Option<&'a str> {
        if !self.declares(parent, name, Some(NodeKind::Module)) {
            return None;
        }
        let parent_path = std::path::Path::new(parent);
        let dir = if parent_is_root {
            parent_path.parent()?.to_path_buf()
        } else {
            rust_module_dir(parent_path)?
        };
        let base = dir.join(name);
        self.file(&base.with_extension("rs"))
            .or_else(|| self.file(&base.join("mod.rs")))
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

    /// What `module` publishes `name` through, as far as `graph.bin` shows:
    /// it keeps no import visibility, so a `use` naming `name`, or a glob
    /// `use` while the module declares no `name`, may be a `pub use` and
    /// answers [`ladder::Published::Unknown`]. Otherwise the module's own.
    fn published(&self, module: &str, name: &str) -> ladder::Published<'static> {
        let imports = match self.dirty.get(module) {
            Some(file) => FileImports::Parsed(&file.imports),
            None => {
                let Some(rows) = self
                    .graph
                    .file_idx_of(module)
                    .and_then(|idx| self.graph.file_imports(idx))
                else {
                    return ladder::Published::Own;
                };
                FileImports::Archived {
                    rows,
                    pool: self.graph.string_pool.as_slice(),
                    files: self.graph.files.as_slice(),
                }
            }
        };
        let mut glob = false;
        for import in imports.iter() {
            if import.alias.unwrap_or(import.imported_name) == name {
                return ladder::Published::Unknown;
            }
            glob |= import.imported_name == "*";
        }
        if glob && !self.declares(module, name, None) {
            ladder::Published::Unknown
        } else {
            ladder::Published::Own
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
        self.modules.child(parent, name, above.is_empty())
    }

    fn published(&self, _root: &'a str, module: &'a str, name: &str) -> ladder::Published<'_> {
        self.modules.published(module, name)
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
/// type at all, so this rejects them before [`resolve_callee`] collects
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
/// [`resolve_callee`] reads for the dirty file `file_ord`, the caller, and
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

impl<G, A> OverlayCall<'_, '_, '_, '_, '_, G, A> {
    /// [`resolve_callee`] for `site` among the declarations of `names`.
    fn resolve(
        &self,
        site: CallSite<'_>,
        names: &OverlayNames<'_>,
        imports: &[RawImport],
        scope: Scope<'_>,
    ) -> Option<(u32, f32)> {
        let (_, types, qualifiers) = self.names;
        resolve_callee(
            self.graph,
            site,
            self.file_ord,
            imports,
            self.caller,
            &mut self.base_metas.borrow_mut(),
            names,
            types,
            qualifiers,
            self.nodes,
            self.replaced,
            self.dirty_base,
            self.facts,
            scope,
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

    /// A Rust path takes the qualifier tier, the module tree and the
    /// receiver-typing ladder, in the order `ladder::qualified` sets
    /// (`RustTail::Ladder`), memoized per file. No Rust receiver is typed,
    /// so `site` is its lookup.
    fn ladder(
        &self,
        site: CallSite<'k>,
        typed: Option<&str>,
        call: CallPick<'_>,
        heritage: &[String],
    ) -> Resolution {
        let (callables, ..) = self.names;
        let imports = self.file.imports.as_slice();
        let scope = Scope {
            call: Some(call),
            heritage,
            type_lookup: false,
            rust_tail: RustTail::Ladder,
        };
        if self.caller.language == Language::Rust && site.name().contains("::") {
            let key = (site.name(), self.owner, self.heritage.as_ptr());
            let memo = self.module_calls.borrow().get(&key).copied();
            let hit = memo.unwrap_or_else(|| {
                let hit = self.resolve(site, callables, imports, scope);
                self.module_calls.borrow_mut().insert(key, hit);
                hit
            });
            return Resolution::from_ladder(hit);
        }
        let lookup = typed.map_or(site, CallSite::Plain);
        let hit = self.resolve(lookup, callables, imports, scope);
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
        self.resolve(
            CallSite::Plain(member),
            callables,
            &self.file.imports,
            Scope {
                call: None,
                heritage,
                type_lookup: false,
                rust_tail: RustTail::Stop,
            },
        )
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
        let imports: &[RawImport] = if bind_imports {
            &self.file.imports
        } else {
            &[]
        };
        self.resolve(
            CallSite::Plain(path),
            self.names.1,
            imports,
            Scope {
                call: Some(call),
                heritage,
                type_lookup: false,
                rust_tail: RustTail::Stop,
            },
        )
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

/// The index's supertypes pre-pass (`pass1_9_supertypes` then
/// `SymbolTable::build_supertypes`) over the dirty files: each heritage
/// subject (a type, the type an `Impl` names, the owner of a Rust trait-impl
/// method) with its bases resolved as types from the declaring file. A
/// subject that is a clean type keeps its archived bases too, less the
/// traits of the dirty files' archived `impl` blocks
/// ([`OverlayFacts::base_supertypes`]): `graph.bin` records no declaring
/// file per base, only those blocks.
#[allow(clippy::too_many_arguments)]
fn dirty_supertypes(
    graph: &ArchivedZeroCopyGraph,
    files: &[OverlayFileInput],
    nodes: &[ViewNode],
    facts: &OverlayFacts<'_>,
    names: (&OverlayNames<'_>, &OverlayNames<'_>),
    base_metas: &mut FxHashMap<usize, FileMeta>,
    replaced: &FxHashMap<u32, u32>,
    dirty_base: &FxHashSet<u32>,
) -> FxHashMap<u32, Supertypes> {
    let base_len = graph.nodes.len() as u32;
    let mut acc: FxHashMap<u32, Supertypes> = FxHashMap::default();
    let mut start = 0usize;
    for (file_ord, file) in files.iter().enumerate() {
        let first = start;
        start += file.symbols.len();
        if file.symbols.iter().all(|s| s.heritage.is_empty()) {
            continue;
        }
        let caller = FileMeta::from_path(&file.rel_path);
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
            .map(|(id, _)| id)
        };
        let mut subjects: FxHashMap<&str, Option<u32>> = FxHashMap::default();
        let mut bases: FxHashMap<&str, Option<u32>> = FxHashMap::default();
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
                    .or_insert_with(|| resolve(type_name))
            };
            let Some(subject) = subject else {
                continue;
            };
            let entry = acc.entry(subject).or_insert_with(|| {
                (subject < base_len)
                    .then(|| facts.base_supertypes(subject))
                    .flatten()
                    .unwrap_or_default()
            });
            for base in &sym.heritage {
                let resolved = *bases
                    .entry(base.as_str())
                    .or_insert_with(|| resolve(base.as_str()));
                match resolved {
                    None if entry.1.is_none() => entry.1 = Some(entry.0.len() as u32),
                    None => {}
                    Some(id) if id != subject && !entry.0.contains(&id) => entry.0.push(id),
                    Some(_) => {}
                }
            }
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
        Scope {
            call: None,
            heritage: &[],
            type_lookup: true,
            rust_tail: RustTail::Ladder,
        },
    )
}

/// The [`FileMeta`] of the base node `idx`'s file, memoized per file.
fn base_file_meta(
    graph: &ArchivedZeroCopyGraph,
    base_metas: &mut FxHashMap<usize, FileMeta>,
    idx: u32,
) -> FileMeta {
    let file_idx = graph.nodes[idx as usize].file_idx.to_native() as usize;
    *base_metas.entry(file_idx).or_insert_with(|| {
        graph
            .files
            .get(file_idx)
            .map_or_else(FileMeta::default, |f| {
                FileMeta::from_path(f.path.resolve(&graph.string_pool))
            })
    })
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

impl<'a> ladder::QualifierFacts for OverlayQualifiers<'_, 'a, '_> {
    type Decls = OverlayFacts<'a>;

    fn decls(&self) -> &OverlayFacts<'a> {
        self.facts
    }

    fn caller_declares(&self, source: &str, name: &str) -> bool {
        debug_assert_eq!(source, self.facts.scope.dirty()[self.file_ord].rel_path);
        self.qualifiers.same_file.contains(&(self.file_ord, name))
    }

    fn stored_file(&self, path: &str) -> Option<&'a str> {
        self.facts.stored_path(path)
    }

    /// The clean base declarations of a qualifier kind (no dirty-file node,
    /// no later overload) and the overlay's, through [`pick_global`].
    fn global_qualifier(&self, qualifier: &str, caller: FileMeta) -> GlobalPick {
        let facts = self.facts;
        let graph = facts.graph;
        let is_qualifier = ResolveTarget::Qualifier.kind_predicate();
        let base = graph
            .nodes_by_name(qualifier)
            .filter(|&idx| {
                is_qualifier(NodeKind::from(&graph.nodes[idx as usize].kind))
                    && !facts.dirty_base.contains(&idx)
                    && !graph.is_later_overload(idx)
            })
            .map(|idx| {
                let meta = facts
                    .node_file(idx)
                    .map_or_else(FileMeta::default, FileMeta::from_path);
                (idx, meta)
            });
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
        debug_assert!(
            target == self.names.target,
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

/// The index's `lookup_member_in_file` over the kind family of `names`: the
/// first `member` declared in `file` whose owner satisfies `owner`. A dirty
/// file answers from its fresh parse only.
#[allow(clippy::too_many_arguments)]
fn member_in_file(
    graph: &ArchivedZeroCopyGraph,
    names: &OverlayNames<'_>,
    nodes: &[ViewNode],
    dirty_base: &FxHashSet<u32>,
    facts: &OverlayFacts<'_>,
    file: &str,
    member: &str,
    owner: MemberOwner<'_>,
) -> Option<u32> {
    if owner == MemberOwner::Any {
        // The match the same-file pick takes with no owner: a free
        // function, else one type's member.
        let pick =
            reexport::pick_call(facts.decls_in_file(file, member).into_iter(), names.target)?;
        return prefer_owned(
            facts,
            file,
            pick,
            member,
            None,
            &[],
            names.target,
            PickScope::default(),
        );
    }
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
pub(super) fn is_relative_specifier(specifier: &str) -> bool {
    specifier.starts_with("./") || specifier.starts_with("../")
}

/// Where `path` falls in the index's probe order for `specifier` of
/// `importer` ([`reexport::for_each_specifier_candidate`] with the index's
/// path `aliases`). `None` when the index never probes it.
pub(super) fn relative_module_rank(
    importer: &str,
    specifier: &str,
    path: &str,
    aliases: &PathAliases,
) -> Option<usize> {
    let mut rank = 0;
    let mut found = None;
    reexport::for_each_specifier_candidate(
        std::path::Path::new(importer),
        specifier,
        |expanded| {
            if !aliases.is_empty() {
                aliases.expand(specifier, |alias| expanded(alias));
            }
        },
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

/// The `member_twins` table [`super::OverlayView::build_with_removed`] fills.
pub(super) type MemberTwins = FxHashMap<u32, (usize, Vec<(u32, Span)>)>;

/// The overlay's edges for every dirty file: `Calls`, lexical closure
/// `References`, name refs, enum-variant refs and field reads. `removed`
/// holds the base paths gone from the worktree: no module lookup finds one.
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
    let file_metas: Vec<FileMeta> = files
        .iter()
        .map(|f| FileMeta::from_path(&f.rel_path))
        .collect();

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
    // walk these bases.
    let _ = facts.dirty_supertypes.set(dirty_supertypes(
        graph,
        files,
        nodes,
        &facts,
        (&types, &qualifiers),
        &mut base_metas,
        replaced,
        dirty_base,
    ));
    // The pre-pass's own qualified lookups ran before the dirty bases
    // existed, and the same-file pick's hierarchy rule walks them.
    // Tier 3's memo reads no bases and stays.
    facts.qualified.borrow_mut().clear();
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
    for (file_ord, file) in files.iter().enumerate() {
        let first = (virt_cursor - base_len) as usize;
        emit_closure_references(
            file,
            &nodes[first..first + file.symbols.len()],
            virt_cursor,
            &mut push_edge,
        );
        // One cache per dirty file: a repeated qualified call reuses
        // both successful and unresolved lookups without rebuilding paths.
        let module_calls: RefCell<ModuleCalls<'_>> = RefCell::default();
        let mut variant_cache = VariantCache::default();
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
                // only the named-import tier. A chain walks from its
                // innermost call outwards, as the index's does.
                // Inline, so `parsed` borrows `sym.calls` for as long
                // as `module_calls` keeps its name.
                let mut chain_failed = false;
                // The two chain callbacks run sequentially and share
                // the same file-metadata cache.
                let chain_metas = std::cell::RefCell::new(&mut base_metas);
                let hit = CallSite::resolve_chain(
                    raw_callee,
                    |parsed, arguments| 'hit: {
                        if let Some(targets) = lexical.call_targets(span_of(sym), parsed) {
                            if let Some(&hit) = targets.first() {
                                break 'hit Some(hit);
                            }
                            // The index's `Resolver::resolve_imported_call`:
                            // the import tiers alone, for the callee and
                            // then for the type it constructs.
                            let site = parsed.fallback();
                            if matches!(site, CallSite::UntypedMember(_)) {
                                break 'hit None;
                            }
                            let import_scoped = |name: &str, target: ResolveTarget| {
                                facts
                                    .named_import(file_ord, &file.imports, name, target)
                                    .map(|decl| (decl, CONF_IMPORT_SCOPED))
                            };
                            let mut resolution = Resolution::from_ladder(import_scoped(
                                site.name(),
                                callables.target,
                            ));
                            if resolution.call_fallbacks_allowed() {
                                // `named_import` memoizes per file, so the
                                // retry reads the file's own imports: only
                                // TS / JS has a lexical scope, and neither
                                // drops them for the last segment.
                                if let Some((ty, confidence)) = ladder::constructed_type(
                                    &facts,
                                    caller.language,
                                    site,
                                    &file.imports,
                                    TypeName::LastSegment,
                                    |name| names_constructible(graph, &types, name),
                                    |name, _| import_scoped(name, ResolveTarget::Type),
                                ) {
                                    let target = *construction_targets
                                        .borrow_mut()
                                        .entry(ty)
                                        .or_insert_with(|| {
                                            construction_target(graph, nodes, type_ctors, ty)
                                        });
                                    resolution =
                                        Resolution::from_ladder(Some((target, confidence)));
                                }
                            }
                            break 'hit resolution.edge();
                        }
                        // The index's `Resolver::resolve_call`: the shared
                        // call entry, which normalizes the site and derives
                        // the call pick from it.
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
                            },
                            parsed,
                        );
                        let mut resolution = verdict.resolution;
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
                                    resolve_callee(
                                        graph,
                                        CallSite::Plain(member),
                                        file_ord,
                                        &file.imports,
                                        caller,
                                        &mut chain_metas.borrow_mut(),
                                        &callables,
                                        &types,
                                        &qualifiers,
                                        nodes,
                                        replaced,
                                        dirty_base,
                                        &facts,
                                        Scope {
                                            call: None,
                                            heritage,
                                            type_lookup: false,
                                            rust_tail: RustTail::Stop,
                                        },
                                    )
                                    .filter(|&(idx, _)| is_free(&facts, idx))
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
                if let Some((target, confidence)) = hit.filter(|&(t, _)| t != source) {
                    // The index's Pass 2 names the reason by the same
                    // confidence (`CallReasons::of`).
                    let reason = if confidence == NEAREST_COPY_CONFIDENCE {
                        NEAREST_COPY_REASON
                    } else if confidence == csharp_extension::HEURISTIC_CONFIDENCE {
                        csharp_extension::HEURISTIC_REASON
                    } else {
                        super::super::merged::OVERLAY_EDGE_REASON
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

            // The index's Pass 2 `field_reads` loop. `variant_refs`
            // is its legacy field name: v5 preserves every raw read,
            // including tagged name references and ordinary fields.
            let mut emitted: Vec<(u32, bool)> = Vec::new();
            let mut name_edges: Vec<u32> = Vec::new();
            let mut field_edges: Vec<u32> = Vec::new();
            for raw in &sym.variant_refs {
                if let Some(name_ref) = NameRef::parse(raw) {
                    // The index's lexical name-reference tier, then
                    // its name tiers with the same owner.
                    let Some((target, confidence)) = resolve_overlay_name_ref(
                        graph,
                        file_ord,
                        file,
                        caller,
                        match root == source {
                            true => sym.owner_class.as_deref(),
                            false => owner,
                        },
                        name_ref,
                        lexical.name_ref_target(span_of(sym), name_ref),
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
                    ) else {
                        continue;
                    };
                    let callable = facts.kind(target).is_callable();
                    // The index's `SymbolTable::twin_read_from`.
                    let target = match callable {
                        true => target,
                        false => twin_read_from(target, file_ord, HeritageNode::span(sym)),
                    };
                    if target == source || name_edges.contains(&target) {
                        continue;
                    }
                    name_edges.push(target);
                    let (rel_type, reason) = if callable {
                        (RelType::References, FUNCTION_REFERENCE_REASON)
                    } else {
                        (RelType::ReadsField, VALUE_READ_REASON)
                    };
                    push_edge(ViewEdge {
                        source,
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
                    if target == source || emitted.contains(&(target, variant_ref.pattern)) {
                        continue;
                    }
                    emitted.push((target, variant_ref.pattern));
                    push_edge(ViewEdge {
                        source,
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
                let hit = match ladder::field_read(&facts, &file.rel_path, raw) {
                    FieldRead::Bound(hit) => Some((hit.id, hit.confidence)),
                    FieldRead::Qualified => resolve_callee(
                        graph,
                        CallSite::Plain(raw),
                        file_ord,
                        &file.imports,
                        caller,
                        &mut base_metas,
                        &fields,
                        &types,
                        &qualifiers,
                        nodes,
                        replaced,
                        dirty_base,
                        &facts,
                        Scope {
                            call: None,
                            heritage: &[],
                            type_lookup: false,
                            rust_tail: RustTail::Stop,
                        },
                    ),
                    FieldRead::Global => global_tier(
                        graph,
                        raw,
                        file_ord,
                        &file.imports,
                        caller,
                        &mut base_metas,
                        &fields,
                        nodes,
                        replaced,
                        dirty_base,
                        &facts,
                        (None, false),
                        &OnceCell::new(),
                    ),
                };
                let Some((target, confidence)) = hit else {
                    continue;
                };
                let target = twin_read_from(target, file_ord, HeritageNode::span(sym));
                if target == source || field_edges.contains(&target) {
                    continue;
                }
                field_edges.push(target);
                push_edge(ViewEdge {
                    source,
                    target,
                    rel_type: RelType::ReadsField,
                    confidence,
                    reason: "field_read",
                });
            }
        }
    }
    lexical_parents
}
