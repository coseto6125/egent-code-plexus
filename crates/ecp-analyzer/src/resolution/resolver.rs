//! Symbol resolver — maps call-site names to global node ids.
//!
//! ## T1-6: FxHashMap uid lookup — decision record
//!
//! The roadmap item T1-6 asked whether a `FxHashMap<u64, NodeId>` field on
//! `SymbolTable` would yield a meaningful win.
//!
//! **`SymbolTable` does not hold `u64` uids at all.** The `uid: u64` hash
//! (xxh3-64 of kind + path + owner_class + name) lives in
//! `ZeroCopyGraph::Node.uid`, which is the serialised on-disk graph consulted
//! at *query time*. `SymbolTable` operates at *build time* with dense
//! sequential `NodeId (u32)` indices and is already O(1) for all its lookups
//! via `FxHashMap<String, …>`. Adding a `u64` uid field to `SymbolTable` would
//! require wiring uid computation into the builder before `current_node_idx`
//! is assigned, with no caller benefit — the resolver tiers never look up by
//! uid, only by (file_path, name) or (name) pairs.
//!
//! **The real O(N) gap** was in the query layer:
//! `impact::classify_symbol` (ecp-cli) did a linear scan over all graph nodes
//! per BFS caller entry to reverse-look-up `uid_string → node_idx`:
//!
//! ```text
//! graph.nodes.iter().enumerate()
//!     .find(|(_, n)| n.uid.to_native().to_string() == caller_uid)
//! ```
//!
//! This is O(N) per caller × O(symbols) per coverage analysis, which on a
//! 3k-node graph with 5 callers/symbol costs ~2 ms per call vs ~0.6 µs with
//! the FxHashMap path (>3000× speedup).
//!
//! **Fix**: `ecp_core::graph_query::build_uid_index` builds
//! `FxHashMap<u64, u32>` once per coverage-analysis call; callers are looked
//! up in O(1). `coverage_analyses` (impact.rs) now builds the table once and
//! passes it to `classify_symbol`.
//!
//! See `crates/ecp-analyzer/tests/resolver_fxhash_uid.rs` for the correctness
//! regression suite and `crates/ecp-analyzer/benches/resolver_lookup.rs` for
//! the before/after bench numbers.

use ecp_core::analyzer::csharp_access::{self, CallAccess};
use ecp_core::analyzer::csharp_extension::{self, ReceiverSite};
use ecp_core::analyzer::import_binding::{
    fqn_imported_name, fqn_language, fqn_type_path, import_binding, import_member_fallback,
    MemberOwner,
};
use ecp_core::analyzer::ladder::{
    self, qualifier_prefix_is_internal, split_qualifier, CallContext, CallPick, CrateModule,
    FieldRead, StemFile, TypeName,
};
use ecp_core::analyzer::module_discovery::{self, Module, ModuleIndex};
use ecp_core::analyzer::outcome::{NoEdge, Resolution, Tier};
use ecp_core::analyzer::path_aliases::PathAliases;
use ecp_core::analyzer::reexport::{
    self, is_ecmascript, probe_rust_module, ExportAliasTable, Fact, FileImports, ImportRef,
    ModuleFacts, PyModule,
};
use ecp_core::analyzer::rust_paths::{
    is_rust_source, rust_expand_path_head, rust_module_path_base, rust_type_path_prefix,
};
use ecp_core::analyzer::type_ladder::{self, TypeCandidates, TypeMember as LadderMember};
use ecp_core::analyzer::types::{owner_key, CallArguments, CallSite, LocalGraph, RawImport};
use ecp_core::graph::NodeKind;
use serde::Serialize;
use std::borrow::Cow;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::resolution::index::{
    CsharpExtensionDecls, FileMeta, GlobalPick, Language, ResolveTarget, SymbolTable,
};
use crate::rust::module_tree::{IndexModuleTree, RustWorkspaceModTree};
use rustc_hash::{FxHashMap, FxHashSet};

pub type NodeId = u32;

type ModuleCache = FxHashMap<
    std::mem::Discriminant<Language>,
    FxHashMap<PathBuf, FxHashMap<String, Module<Arc<str>>>>,
>;

/// What the import tier reads to follow a module's re-export one hop: the
/// imports of each file, and the modules
/// each TS / JS file's `export * from` names. Built once per index build,
/// after Pass 1, and shared by every resolver of that build.
#[derive(Default)]
pub struct ReexportIndex<'g> {
    modules: FxHashMap<Cow<'g, str>, ModuleExports<'g>>,
    /// Local names that a TS / JS import or `export { x as y } from` gives
    /// to a declaration of another name (a default import included), and
    /// the export names a module gives its own declarations (`export { g as
    /// f }`). A TS / JS re-export reaches a name only when the symbol table
    /// declares it or this set holds it.
    ecmascript_aliases: FxHashSet<&'g str>,
}

struct ModuleExports<'g> {
    imports: &'g [RawImport],
    star_modules: Box<[Box<str>]>,
}

impl<'g> ReexportIndex<'g> {
    pub fn build(
        local_graphs: &'g [LocalGraph],
        symbol_table: &SymbolTable,
        path_aliases: &PathAliases,
    ) -> Self {
        let resolver = Resolver::new(symbol_table).with_path_aliases(path_aliases.clone());
        let facts = IndexFacts(&resolver);
        let mut ecmascript_aliases: FxHashSet<&'g str> = local_graphs
            .iter()
            .filter_map(|graph| graph.export_aliases.as_deref())
            .flatten()
            .map(|alias| &*alias.exported)
            .collect();
        let modules = local_graphs
            .iter()
            .filter(|graph| !graph.imports.is_empty())
            .map(|graph| {
                let path = normalize_source_path(&graph.file_path);
                let language = Language::from_normalized_path(&path);
                if is_ecmascript(language) {
                    ecmascript_aliases.extend(graph.imports.iter().filter_map(|import| {
                        let import = ImportRef::from(import);
                        if import.is_es_default() {
                            Some(import.imported_name)
                        } else {
                            import.alias.filter(|_| import.imported_name != "*")
                        }
                    }));
                }
                let star_modules = if is_ecmascript(language) {
                    reexport::resolve_star_modules(
                        &facts,
                        &path,
                        graph.imports.iter().map(ImportRef::from),
                    )
                } else {
                    Box::default()
                };
                (
                    path,
                    ModuleExports {
                        imports: &graph.imports,
                        star_modules,
                    },
                )
            })
            .collect();
        Self {
            modules,
            ecmascript_aliases,
        }
    }

    /// Whether a TS / JS re-export can reach a declaration under `name`.
    fn may_reach(&self, symbol_table: &SymbolTable, name: &str) -> bool {
        symbol_table.global_match_count(name) > 0 || self.ecmascript_aliases.contains(name)
    }
}

/// The index's [`ModuleFacts`]: the symbol table, and the re-export index
/// when the resolver follows re-exports.
struct IndexFacts<'r, 'a>(&'r Resolver<'a>);

impl ModuleFacts for IndexFacts<'_, '_> {
    type Path = Arc<str>;

    fn has_file(&self, file: &str) -> bool {
        self.0.symbol_table.has_indexed_file(file)
    }

    fn files_in_directory(&self, language: Language, directory: &str) -> Vec<Self::Path> {
        self.0
            .symbol_table
            .files_in_directory(directory)
            .iter()
            .filter(|file| Language::from_normalized_path(file) == language)
            .map(|file| Arc::from(file.as_str()))
            .collect()
    }

    fn namespace_files(&self, language: Language, namespace: &str) -> Vec<Self::Path> {
        self.0
            .symbol_table
            .namespace_files(namespace)
            .iter()
            .filter(|file| Language::from_normalized_path(file) == language)
            .map(|file| Arc::from(file.as_str()))
            .collect()
    }

    fn has_module_name(&self, language: Language, name: &str) -> bool {
        self.0.symbol_table.has_module_name(language, name)
    }

    fn declares(&self, file: &str, name: &str) -> bool {
        self.0.symbol_table.lookup_in_file(file, name).is_some()
    }

    fn declares_top_level_module(&self, file: &str, name: &str) -> bool {
        self.decls_in_file(file, name)
            .into_iter()
            .any(|(id, kind)| {
                kind == NodeKind::Module
                    && ecp_core::analyzer::same_file_pick::PickFacts::owner(self.0.symbol_table, id)
                        .is_none()
            })
    }

    fn decls_in_file(&self, file: &str, name: &str) -> Vec<(u32, NodeKind)> {
        self.0.symbol_table.decls_in_file(file, name)
    }

    fn is_exported(&self, decl: u32) -> bool {
        self.0.symbol_table.is_exported(decl)
    }

    fn is_es_module(&self, file: &str) -> bool {
        self.0.symbol_table.is_es_module(file)
    }

    fn export_aliases(&self, file: &str) -> Option<Fact<'_, ExportAliasTable>> {
        self.0.symbol_table.export_aliases(file).map(Fact::Borrowed)
    }

    fn imports(&self, file: &str) -> Option<FileImports<'_>> {
        let exports = self.0.reexports?.modules.get(file)?;
        Some(FileImports::Parsed(exports.imports))
    }

    fn star_modules(&self, module: &str) -> Fact<'_, [Box<str>]> {
        match self.0.reexports.and_then(|index| index.modules.get(module)) {
            Some(exports) => Fact::Borrowed(&exports.star_modules[..]),
            None => Fact::Borrowed(&[]),
        }
    }

    fn python_module(&self, importer: &str, specifier: &str) -> PyModule<Arc<str>> {
        match self
            .0
            .import_module_file(Path::new(importer), specifier, Language::Python)
        {
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
        self.0
            .for_each_candidate(Path::new(importer), specifier, |candidate| visit(candidate));
    }
}

/// The index's [`ModuleIndex`]: the symbol table's files, and the resolver's
/// candidates (path aliases, Rust module tree).
impl<'a> ModuleIndex<'a> for IndexFacts<'_, 'a> {
    fn module_file(&self, language: Language, path: &str) -> Option<&'a str> {
        if Language::from_normalized_path(path) != language {
            return None;
        }
        let table: &'a SymbolTable = self.0.symbol_table;
        table.indexed_file_path(path)
    }

    fn module_files_with_stem(
        &self,
        language: Language,
        stem: &str,
    ) -> impl Iterator<Item = &'a str> {
        let table: &'a SymbolTable = self.0.symbol_table;
        table
            .module_files_by_stem(stem)
            .filter(move |file| Language::from_normalized_path(file) == language)
    }

    fn namespace_declarers(&self, language: Language, name: &str) -> Vec<&'a str> {
        let table: &'a SymbolTable = self.0.symbol_table;
        table
            .namespace_files(name)
            .iter()
            .map(String::as_str)
            .filter(|file| Language::from_normalized_path(file) == language)
            .collect()
    }

    fn module_declares(&self, file: &str, name: &str) -> bool {
        self.0.symbol_table.lookup_in_file(file, name).is_some()
    }

    fn declares_other_namespace(&self, file: &str, namespace: &str) -> bool {
        self.0
            .symbol_table
            .declares_other_namespace(file, namespace)
    }

    fn for_each_module_candidate(
        &self,
        importer: &str,
        specifier: &str,
        visit: impl FnMut(&str) -> bool,
    ) {
        self.0
            .for_each_candidate(Path::new(importer), specifier, visit);
    }
}

/// The index's module discovery for the shared import tiers
/// ([`ladder::import_member`]), through [`Resolver::import_module_file`]'s memo.
impl ladder::ImportModules for IndexFacts<'_, '_> {
    type Module = Arc<str>;

    fn import_module(
        &self,
        importer: &str,
        specifier: &str,
        language: Language,
    ) -> Module<Arc<str>> {
        self.0
            .import_module_file(Path::new(importer), specifier, language)
    }

    fn module_name_indexed(&self, language: Language, name: &str) -> bool {
        self.0.symbol_table.has_module_name(language, name)
    }
}

impl ladder::ImportMembers for IndexFacts<'_, '_> {
    fn member_in_file(
        &self,
        file: &str,
        name: &str,
        target: ResolveTarget,
        owner: MemberOwner<'_>,
    ) -> Option<u32> {
        self.0
            .symbol_table
            .lookup_member_in_file(file, name, target, owner)
    }

    fn python_reexport(
        &self,
        language: Language,
        module: &str,
        name: &str,
        target: ResolveTarget,
    ) -> Option<u32> {
        reexport::python_import_reexport(self, language, module, name, target)
    }
}

/// The index's facts for the qualifier tier ([`ladder::qualified`]): the
/// symbol table, the resolver's candidates and its Rust module tree.
impl ladder::QualifierFacts for IndexFacts<'_, '_> {
    type Decls = SymbolTable;

    fn decls(&self) -> &SymbolTable {
        self.0.symbol_table
    }

    fn caller_declares(&self, source: &str, name: &str) -> bool {
        self.0
            .symbol_table
            .lookup_in_file_with_kind(source, name, ResolveTarget::Qualifier)
            .is_some()
    }

    fn stored_file(&self, path: &str) -> Option<Arc<str>> {
        Some(Arc::from(path))
    }

    fn global_qualifier(&self, qualifier: &str, caller: FileMeta) -> GlobalPick {
        self.0
            .symbol_table
            .lookup_global(qualifier, ResolveTarget::Qualifier, caller, false)
    }

    fn for_each_candidate(&self, importer: &str, specifier: &str, visit: impl FnMut(&str) -> bool) {
        self.0
            .for_each_candidate(Path::new(importer), specifier, visit);
    }

    fn exports(&self, file: &str, name: &str, target: ResolveTarget) -> bool {
        reexport::lookup_export_in_file(self, file, name, target).is_some()
    }

    fn holds_import(&self, file: &str, imported: &str, default_import: bool) -> bool {
        reexport::imported_in_file(
            self,
            file,
            imported,
            default_import,
            ResolveTarget::Qualifier,
        )
        .is_some()
    }

    fn rust_module_file(&self, source: &str, module_path: &str) -> Option<Arc<str>> {
        self.0
            .rust_module_file(Path::new(source), module_path)
            .map(Arc::from)
    }

    /// The module tree places `<crate>::a::m` (`use other::m; m::f()`),
    /// which `rust_module_file` cannot; past it only file-stem guesses
    /// remain, and they pick the caller crate's own `m.rs`.
    fn crate_module(
        &self,
        source: &str,
        module_path: &str,
        member: &str,
        target: ResolveTarget,
    ) -> CrateModule<Arc<str>> {
        let resolver = self.0;
        let Some(tree) = resolver.module_tree() else {
            return CrateModule::Local;
        };
        match ladder::crate_module(
            &tree,
            resolver.symbol_table,
            source,
            module_path,
            member,
            target,
        ) {
            CrateModule::Local => CrateModule::Local,
            CrateModule::Placed(file) => CrateModule::Placed(file.map(Arc::from)),
        }
    }

    fn rust_path_agrees(
        &self,
        source: &str,
        callee: &str,
        qualifier: &str,
        candidate: &str,
        imports: &[RawImport],
    ) -> Option<bool> {
        Some(self.0.rust_type_path_agrees(
            Path::new(source),
            source,
            Some(callee),
            qualifier,
            candidate,
            imports,
        ))
    }

    fn files_by_stem(&self, stem: &str) -> impl Iterator<Item = &str> {
        self.0
            .symbol_table
            .files_by_stem(stem)
            .iter()
            .map(String::as_str)
    }

    fn type_candidates<'q>(
        &self,
        source: &str,
        name: &'q str,
        imports: &'q [RawImport],
    ) -> TypeCandidates<'q> {
        self.0
            .type_candidates(Path::new(source), source, name, imports)
    }
}

/// The index's facts for one call site's entry ([`ladder::call`]): the
/// resolver, and the caller's file and imports its tiers read.
struct IndexCall<'r, 'a, 'p> {
    facts: IndexFacts<'r, 'a>,
    source_file: &'p Path,
    /// `source_file` with forward slashes.
    source: &'p str,
    language: Language,
    raw_imports: &'p [RawImport],
    caller_owner: Option<&'p str>,
    /// The ladder's list, kept so its allocation becomes the returned one.
    targets: RefCell<Vec<(NodeId, f32)>>,
}

impl<'s, 'r, 'a> ladder::CallFacts<'s> for IndexCall<'r, 'a, '_> {
    type Modules = IndexFacts<'r, 'a>;
    type Decls = SymbolTable;
    const TYPE_NAME: TypeName = TypeName::Qualifier;

    fn modules(&self) -> &IndexFacts<'r, 'a> {
        &self.facts
    }

    fn decls(&self) -> &SymbolTable {
        self.facts.0.symbol_table
    }

    fn type_candidates<'q>(
        &self,
        source: &str,
        receiver: &'q str,
        imports: &'q [RawImport],
    ) -> TypeCandidates<'q> {
        self.facts
            .0
            .type_candidates(self.source_file, source, receiver, imports)
    }

    fn ladder(
        &self,
        site: CallSite<'s>,
        typed: Option<&str>,
        call: CallPick<'_>,
        heritage: &[String],
    ) -> Resolution {
        let targets = self.facts.0.resolve_symbol_with_import_binding(
            self.source_file,
            typed.unwrap_or(site.name()),
            self.language,
            (self.raw_imports, !call.untyped),
            call_target(self.source_file, site),
            heritage,
            Some(call),
            &mut None,
        );
        let resolution = ladder_resolution(&targets);
        *self.targets.borrow_mut() = targets;
        resolution
    }

    fn csharp_admitted(&self, id: u32, site: CallSite<'_>) -> Option<u32> {
        let table = self.facts.0.symbol_table;
        let access = CallAccess::of(site, self.source, self.caller_owner.and_then(owner_key));
        csharp_access::admitted_overload(
            table,
            id,
            table.later_overloads(id),
            |id| table.csharp_method_flags(id),
            access,
        )
    }

    fn bare_member(
        &self,
        member: &str,
        target: ResolveTarget,
        heritage: &[String],
    ) -> Option<(u32, f32)> {
        ladder_resolution(&self.facts.0.resolve_symbol_with_heritage(
            self.source_file,
            member,
            self.raw_imports,
            target,
            heritage,
        ))
        .edge()
    }

    fn names_type_and_member(&self, callee: &str) -> bool {
        let table = self.facts.0.symbol_table;
        table.has_constructible_type(callee)
            && table.names_admitted_method(callee, FileMeta::from_normalized_path(self.source))
    }

    fn declares_constructible(&self, name: &str) -> bool {
        self.facts.0.symbol_table.has_constructible_type(name)
    }

    fn resolve_type(
        &self,
        path: &str,
        bind_imports: bool,
        call: CallPick<'_>,
        heritage: &[String],
    ) -> Option<(u32, f32)> {
        let types = self.facts.0.resolve_symbol_with_import_binding(
            self.source_file,
            path,
            self.language,
            (self.raw_imports, bind_imports && !call.untyped),
            ResolveTarget::Type,
            heritage,
            Some(call),
            &mut None,
        );
        sole_target(&types)
    }

    fn construction_target(&self, type_id: u32) -> u32 {
        self.facts
            .0
            .symbol_table
            .sole_constructor(type_id)
            .unwrap_or(type_id)
    }

    fn import_tiers(
        &self,
        name: &str,
        target: ResolveTarget,
        bind_imports: bool,
    ) -> Option<(u32, f32)> {
        sole_target(&self.facts.0.resolve_import_tiers(
            self.source_file,
            self.language,
            name,
            (self.raw_imports, bind_imports),
            target,
            &mut None,
        ))
    }

    fn note_import_bound(&self, name: &str, import: &RawImport, id: u32) {
        let _ = self
            .facts
            .0
            .import_scoped(self.source_file, name, import, id);
    }

    fn note_untyped_unresolved(&self, name: &str) {
        if self.facts.0.decisions.is_some() {
            self.facts.0.record(
                self.source,
                name,
                None,
                DecisionTier::Unresolved,
                None,
                0,
                None,
            );
        }
    }
}

#[cfg(not(windows))]
#[inline]
fn normalize_source_path(path: &Path) -> Cow<'_, str> {
    path.to_string_lossy()
}

#[cfg(windows)]
#[inline]
fn normalize_source_path(path: &Path) -> Cow<'_, str> {
    Cow::Owned(path.to_string_lossy().replace('\\', "/"))
}

/// Resolver outcome tier captured per `resolve_symbol` call when the dump
/// is enabled. Distinct from [`ResolutionTier`] because that enum models
/// only resolution *successes*,
/// whereas the dump also needs to record the `Unresolved` outcome to let
/// the verification harness compute false negatives.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum DecisionTier {
    SameFile,
    ImportScoped,
    /// Tier 2.5 — qualifier-scoped lookup succeeded (see
    /// [`ResolutionTier::QualifierScoped`]).
    QualifierScoped,
    /// Tier 2.75 — heritage-scoped lookup succeeded (see
    /// [`ResolutionTier::HeritageScoped`]).
    HeritageScoped,
    Global,
    /// Tier 3 produced ≥2 kind-filtered candidates and suppressed the edge.
    /// Distinct from `Unresolved` (=0 candidates) so the verification
    /// harness can tell "no defence needed" from "defence fired" without
    /// needing to inspect `alt_count`. Edge behaviour is unchanged — both
    /// outcomes emit no edge.
    AmbiguousGlobal,
    /// Tier 3.5 — Rust workspace module-tree resolved the FQN path to a
    /// concrete file and found the member there. Confidence 1.0; tagged
    /// `reason: "module-tree"` so analytics can distinguish from Tier-4
    /// heuristic edges (confidence 0.7).
    ModuleTree,
    Unresolved,
    /// Receiver-typing ladder: the qualifier names one project type, and
    /// that type owns the member (in its file, or for Rust impls / Go
    /// methods / C# partials / Swift extensions, in its crate or package).
    TypeOwned,
    /// Receiver-typing ladder: a resolved declared supertype of the
    /// qualifier's one project type owns the member.
    TypeHeritage,
    /// Receiver-typing ladder: the qualifier names several project types;
    /// exactly one owns the member and every other provably does not.
    TypeCandidates,
    /// A call whose candidates are copies of one declaration in duplicated
    /// source trees took the copy nearest the caller by path (see
    /// [`ResolutionTier::NearestCopy`]), at the ladder or at Tier 3.
    NearestCopy,
}

/// One resolver attempt, captured when the dump buffer is enabled. The
/// builder serializes a sibling JSONL view of these (resolving
/// `target_id → target_file` via [`SymbolTable::file_of`]) — see
/// `docs/specs/2026-05-15-resolver-oracle-harness.md`.
#[derive(Debug, Clone, Serialize)]
pub struct ResolverDecision {
    pub src_file: String,
    pub name: String,
    pub specifier: Option<String>,
    pub tier: DecisionTier,
    pub target_id: Option<NodeId>,
    pub alt_count: u32,
    pub confidence: Option<f32>,
}

/// The core resolver engine that matches symbol names to concrete global nodes.
pub struct Resolver<'a> {
    symbol_table: &'a SymbolTable,
    module_cache: RwLock<ModuleCache>,
    /// `None` on the production path → zero-cost (single Option-discriminant
    /// branch in `record`, no `Mutex` touch). `Some(_)` only when the
    /// builder enabled dumping via [`Resolver::enable_dump`].
    // `Mutex` (not `RefCell`) so the whole `Resolver` is `Sync` and can be
    // shared across rayon workers. In the production path `decisions` is
    // `None` and the `Option::Some` guard in `record()` short-circuits
    // before any lock — Mutex overhead is paid only when --dump-resolver
    // is on (a debug-only flag, currently no-op in v2 layout).
    decisions: Option<Mutex<Vec<ResolverDecision>>>,
    /// Module-specifier aliases sourced from project config (TS
    /// `tsconfig.json` `compilerOptions.paths`, etc.). Consulted during
    /// Tier 2 import resolution before the relative-resolution fallback so
    /// `@/utils` maps to `src/utils` (then existing extension/index
    /// probing finishes the lookup).
    path_aliases: PathAliases,
    /// Rust workspace module tree for Tier 3.5 FQN resolution.
    /// `None` when not available (non-Rust repos, no Cargo.toml, build
    /// failure). Shared across rayon workers — read-only after construction.
    mod_tree: Option<&'a RustWorkspaceModTree>,
    /// Workspace root path, used by Tier 3.5 to make absolute paths
    /// repo-relative. `None` when `mod_tree` is `None`.
    workspace_root: Option<std::path::PathBuf>,
    /// `None` follows no re-export: the import tier then binds only a
    /// member the imported module declares itself.
    reexports: Option<&'a ReexportIndex<'a>>,
    /// The C# extension declarations by name, built once per Pass 2
    /// ([`SymbolTable::csharp_extension_decl_map`]). `None` reads them from
    /// the table per call.
    csharp_extensions: Option<&'a CsharpExtensionDecls<'a>>,
}

impl<'a> Resolver<'a> {
    /// Creates a new `Resolver` with a reference to the global `SymbolTable`.
    pub fn new(symbol_table: &'a SymbolTable) -> Self {
        Self {
            symbol_table,
            module_cache: RwLock::new(ModuleCache::default()),
            decisions: None,
            path_aliases: PathAliases::new(),
            mod_tree: None,
            workspace_root: None,
            reexports: None,
            csharp_extensions: None,
        }
    }

    /// Read the C# extension declarations from `decls`, the table's
    /// [`SymbolTable::csharp_extension_decl_map`], instead of rebuilding a
    /// name's list per failing call.
    pub(crate) fn with_csharp_extensions(mut self, decls: &'a CsharpExtensionDecls<'a>) -> Self {
        self.csharp_extensions = Some(decls);
        self
    }

    /// Let the import tier follow Python package and TS / JS re-exports one
    /// hop through `reexports`.
    pub fn with_reexports(mut self, reexports: &'a ReexportIndex<'a>) -> Self {
        self.reexports = Some(reexports);
        self
    }

    /// Replace the resolver's empty default alias set. Used by the builder
    /// to forward project-level config (`tsconfig.json` etc.) into the
    /// Tier-2 specifier expansion.
    pub fn with_path_aliases(mut self, aliases: PathAliases) -> Self {
        self.path_aliases = aliases;
        self.module_cache.get_mut().unwrap().clear();
        self
    }

    /// Attach the Rust workspace module tree for Tier 3.5 FQN resolution.
    pub fn with_mod_tree(
        mut self,
        tree: &'a RustWorkspaceModTree,
        workspace_root: std::path::PathBuf,
    ) -> Self {
        self.mod_tree = Some(tree);
        self.workspace_root = Some(workspace_root);
        self
    }

    /// Turn on the decision recorder. Each subsequent `resolve_symbol` call
    /// pushes a [`ResolverDecision`] into the internal buffer.
    pub fn enable_dump(&mut self) {
        self.decisions = Some(Mutex::new(Vec::new()));
    }

    /// Drain the recorded decisions. Returns `None` if dumping was never
    /// enabled.
    pub fn take_decisions(&mut self) -> Option<Vec<ResolverDecision>> {
        self.decisions
            .take()
            .map(|m| m.into_inner().unwrap_or_default())
    }

    /// Enumerate candidate target file paths for an import specifier, walking
    /// the same expansion rules used internally by Tier 2 resolution
    /// (path-alias expansion, relative-resolution, Python-style dotted,
    /// extension/index suffix probing). The visitor is called once per
    /// candidate path string and may return `false` to stop early.
    ///
    /// Exposed for `post_process::imports_edges`, which needs to resolve
    /// module-style imports (e.g. Ruby `require_relative 'alpha'`, Go
    /// `import "x/pkg"`) to a File node target when no named symbol
    /// matches `RawImport.imported_name`.
    pub fn enumerate_candidates<F>(&self, source_file: &std::path::Path, specifier: &str, visit: F)
    where
        F: FnMut(&str) -> bool,
    {
        self.for_each_candidate(source_file, specifier, visit);
    }

    /// [`for_each_specifier_candidate`], except that a Rust `crate` / `self`
    /// / `super` path the module tree places is visited as that one file.
    /// The tree knows which Cargo target declares the caller (`crate` in a
    /// module of `src/main.rs` is `main.rs`, not `lib.rs`) and which module
    /// a `#[path]` file is; the file-layout guess knows neither, so it does
    /// not run after the tree has answered.
    fn for_each_candidate<F>(&self, source_file: &Path, specifier: &str, mut visit: F)
    where
        F: FnMut(&str) -> bool,
    {
        if let Some(file) = self.anchored_rust_module(source_file, specifier) {
            visit(file);
            return;
        }
        for_each_specifier_candidate(source_file, specifier, &self.path_aliases, visit);
    }

    /// [`module_discovery::discover_module`] over this index, memoized per
    /// language, caller directory (relative specifiers only) and specifier.
    /// `pub` for `crates/ecp-cli/tests/module_discovery_parity.rs`, which
    /// feeds one file set to this and to the overlay's `ImportScope`.
    pub fn import_module_file(
        &self,
        source_file: &Path,
        specifier: &str,
        language: Language,
    ) -> Module<Arc<str>> {
        let directory = if specifier.starts_with('.') {
            source_file.parent().unwrap_or(Path::new(""))
        } else {
            Path::new("")
        };
        if let Some(hit) = self
            .module_cache
            .read()
            .unwrap()
            .get(&std::mem::discriminant(&language))
            .and_then(|directories| directories.get(directory))
            .and_then(|imports| imports.get(specifier))
        {
            return hit.clone();
        }
        let mut cache = self.module_cache.write().unwrap();
        cache
            .entry(std::mem::discriminant(&language))
            .or_default()
            .entry(directory.to_path_buf())
            .or_default()
            .entry(specifier.to_string())
            .or_insert_with(|| {
                module_discovery::discover_module(
                    &IndexFacts(self),
                    &normalize_source_path(source_file),
                    specifier,
                    language,
                )
                .map(|&file| Arc::from(file))
            })
            .clone()
    }

    /// The module tree's file for the `crate` / `self` / `super`-anchored
    /// Rust module path `module_path`, as seen from `source_file`.
    fn anchored_rust_module(&self, source_file: &Path, module_path: &str) -> Option<&'a str> {
        let tree = self.mod_tree?;
        if !matches!(
            module_path.split("::").next(),
            Some("crate" | "self" | "super")
        ) {
            return None;
        }
        tree.anchored_module_file(&normalize_source_path(source_file), module_path)
    }

    /// Resolves a symbol name to possible target nodes with confidence scores.
    ///
    /// `target` constrains Tier-3 (Global) fallback so a bare `format()` /
    /// `new()` doesn't fan out to every same-named symbol in the graph.
    /// Tier-3 returns at most one match — ambiguity → zero edges.
    pub fn resolve_symbol(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
    ) -> Vec<(NodeId, f32)> {
        self.resolve_symbol_with_heritage(source_file, symbol_name, raw_imports, target, &[])
    }

    /// Resolve one call site to its `Calls` targets through the shared call
    /// entry ([`ladder::call`]): the callable tiers, then the call-level
    /// fallbacks, among them the constructor fallback
    /// ([`ladder::constructed_type`]). The fallback lives here, not in
    /// [`Self::resolve_symbol_with_heritage`], because that method also
    /// serves References / Decorates / Imports edges, which must never land
    /// on a type through a construction rule. `caller_owner` is the type the
    /// calling member belongs to, `caller` the calling declaration
    /// (`SymbolTable::prefer_owned`). `lexical` is the file's verdict on the
    /// site (`LexicalFunctions::call_resolution`): a function in scope binds
    /// ahead of every tier, and a name a lexical scope hides takes the
    /// import-only path.
    ///
    /// A member call reaches a method or a type's function, never a type or
    /// its constructor by the type's name: an untyped `o.m()`
    /// ([`CallSite::UntypedMember`]) never takes the constructor fallback,
    /// and every tier's target passes `same_file_pick::member_call_admits` (no
    /// free function, no namespace member, no constructor, no member of the
    /// caller's own type by the name alone). Neither does a bare `m()` that
    /// may be a member call whose receiver the parser dropped take the
    /// fallback.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_call(
        &self,
        source_file: &Path,
        site: CallSite<'_>,
        raw_imports: &[RawImport],
        caller_heritage: &[String],
        caller_owner: Option<&str>,
        caller: Option<NodeId>,
        lexical: Resolution,
    ) -> Vec<(NodeId, f32)> {
        let source = normalize_source_path(source_file);
        let language = Language::from_normalized_path(&source);
        let facts = IndexCall {
            facts: IndexFacts(self),
            source_file,
            source: &source,
            language,
            raw_imports,
            caller_owner,
            targets: RefCell::default(),
        };
        let verdict = ladder::call(
            &facts,
            CallContext {
                source: &source,
                language,
                imports: raw_imports,
                heritage: caller_heritage,
                owner: caller_owner,
                caller,
                lexical,
            },
            site,
        );
        // The ladder's allocation is reused for the one edge.
        let mut targets = facts.targets.into_inner();
        targets.clear();
        targets.extend(verdict.resolution.edge());
        targets
    }

    /// The member `member` of the type a call to `callee` returns
    /// ([`type_ladder::returned_type`]): the receiver of `Foo.create().bar()`
    /// is the return type of the one `create` the inner call binds. The
    /// type name is read in `callee`'s own file, where its signature names
    /// it, and the member is found by the receiver-typing ladder, never by
    /// the name alone. The edge takes the ladder's confidence for a typed
    /// receiver, capped by `inner_confidence`. `None` when `callee` returns
    /// no reducible type or that type owns no `member`. The caller permits
    /// [`CallSite::fallback`] only when the return type is unknown.
    ///
    /// Read imports in the callee's file. Java/Kotlin imports bind a type;
    /// C# namespaces and Swift package targets restrict visible types before
    /// member lookup. Other languages retain the directory-scoped fallback.
    pub(crate) fn resolve_chained_member_outcome(
        &self,
        source_file: &Path,
        (callee, inner_confidence): (NodeId, f32),
        member: &str,
    ) -> type_ladder::ChainOutcome<(NodeId, f32)> {
        let st = self.symbol_table;
        let Some(ty) = type_ladder::returned_type(st, callee) else {
            return type_ladder::ChainOutcome::Unknown;
        };
        let Some(callee_file) = st.file_of(callee) else {
            return type_ladder::ChainOutcome::Unknown;
        };
        let language = Language::from_normalized_path(callee_file);
        let imports = self
            .reexports
            .and_then(|index| index.modules.get(callee_file))
            .map_or(&[][..], |exports| exports.imports);
        let source = Path::new(callee_file);
        let candidates = if let Some(id) =
            st.lookup_in_file_with_kind(callee_file, ty, ResolveTarget::Type)
        {
            TypeCandidates::Unique(id, ty)
        } else {
            match language {
                Language::Java | Language::Kotlin
                    if imports
                        .iter()
                        .any(|import| import_binding(import, ty, language).is_some()) =>
                {
                    let imported =
                        self.resolve_imported_symbol(source, ty, imports, ResolveTarget::Type);
                    match imported.as_slice() {
                        [(id, _)] => {
                            TypeCandidates::Unique(*id, st.name_in_file(*id).unwrap_or(ty))
                        }
                        _ => return type_ladder::ChainOutcome::Conclusive,
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
                    type_ladder::visible_return_type(st, callee_file, name, |id| {
                        st.namespace_type_path_matches(id, path)
                    })
                }
                Language::CSharp => type_ladder::visible_return_type(st, callee_file, ty, |id| {
                    st.namespace_return_type_visible(id, callee_file, imports)
                }),
                Language::Swift => type_ladder::visible_return_type(st, callee_file, ty, |id| {
                    st.file_of(id).is_some_and(|file| {
                        type_ladder::swift_return_type_visible(callee_file, file, imports)
                    })
                }),
                Language::Python | Language::TypeScript | Language::JavaScript => {
                    self.type_candidates(source, callee_file, ty, imports)
                }
                Language::Rust => match reexport::rust_imported_type(
                    &IndexFacts(self),
                    callee_file,
                    ty,
                    imports.iter().map(ImportRef::from),
                ) {
                    reexport::RustImportedType::Unique(id) => {
                        let Some(name) = type_ladder::TypeFacts::name_of(st, id) else {
                            return type_ladder::ChainOutcome::Conclusive;
                        };
                        TypeCandidates::Unique(id, name)
                    }
                    reexport::RustImportedType::External
                    | reexport::RustImportedType::Conclusive => {
                        return type_ladder::ChainOutcome::Conclusive;
                    }
                    reexport::RustImportedType::Unmodeled => {
                        match type_ladder::unmodeled_return_type(st, callee_file, ty) {
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
                    self.type_candidates(source, callee_file, ty, imports)
                }
                Language::Cpp | Language::Dart if !imports.is_empty() => {
                    match reexport::imported_file_type(
                        &IndexFacts(self),
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
                                && st.declares_type_beside(callee_file, ty) =>
                        {
                            self.type_candidates(source, callee_file, ty, &[])
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
                _ if st.declares_type_beside(callee_file, ty) => {
                    self.type_candidates(source, callee_file, ty, &[])
                }
                _ if language == Language::Go
                    && reexport::go_dot_import_blocks_unmodeled_type(
                        imports.iter().map(ImportRef::from),
                    ) =>
                {
                    return type_ladder::ChainOutcome::Unknown;
                }
                _ => match type_ladder::unmodeled_return_type(st, callee_file, ty) {
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
        let target = call_target(source_file, CallSite::UntypedMember(member));
        let Some((id, tier)) = Tier::of_member(type_ladder::member_via_type(
            st,
            candidates,
            callee_file,
            member,
            target,
            true,
        )) else {
            return type_ladder::ChainOutcome::Conclusive;
        };
        type_ladder::ChainOutcome::Bound((id, tier.confidence().min(inner_confidence)))
    }

    /// The method a C# chained member call (`Make().Read(1)`) from a caller
    /// declared by `caller_owner` (an `owner_key`) in `source_file` binds in
    /// place of `target`, the member [`Self::resolve_chained_member_outcome`]
    /// bound: the access rule a plain call applies ([`Self::resolve_call`],
    /// codex ruling P-ext2 C). The receiver is the value the inner call
    /// returns, so the access is an instance call. `None` when C# admits no
    /// overload, or more than one.
    pub(crate) fn csharp_admitted_chain_member(
        &self,
        source_file: &Path,
        caller_owner: Option<&str>,
        target: NodeId,
    ) -> Option<NodeId> {
        let caller_file = normalize_source_path(source_file);
        let access = CallAccess {
            instance: true,
            caller_file: &caller_file,
            caller_owner,
        };
        csharp_access::admitted_overload(
            self.symbol_table,
            target,
            self.symbol_table.later_overloads(target),
            |id| self.symbol_table.csharp_method_flags(id),
            access,
        )
    }

    /// The member a C# call reaches on a value of unknown type, which only
    /// the extension retry ([`ReceiverSite::Untyped`]) may bind: an untyped
    /// member site, or a `FromReceiver` whose receiver names no type
    /// ([`ladder::call`] reads it as untyped). `None` for any other site.
    /// `site` is a C# call's.
    pub(crate) fn csharp_untyped_member<'s>(
        &self,
        source_file: &Path,
        site: CallSite<'s>,
        raw_imports: &[RawImport],
    ) -> Option<&'s str> {
        match site.fallback() {
            CallSite::UntypedMember(member) => Some(member),
            site @ CallSite::FromReceiver { member, .. } => {
                let source = normalize_source_path(source_file);
                let facts = IndexCall {
                    facts: IndexFacts(self),
                    source_file,
                    source: &source,
                    language: Language::CSharp,
                    raw_imports,
                    caller_owner: None,
                    targets: RefCell::default(),
                };
                ladder::type_receiver(&facts, &source, Language::CSharp, raw_imports, site)
                    .is_none()
                    .then_some(member)
            }
            _ => None,
        }
    }

    /// Resolve an explicit import without selecting an inaccessible local binding.
    pub fn resolve_imported_symbol(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
    ) -> Vec<(NodeId, f32)> {
        let language = Language::from_normalized_path(&normalize_source_path(source_file));
        self.resolve_import_tiers(
            source_file,
            language,
            symbol_name,
            (raw_imports, true),
            target,
            &mut None,
        )
    }

    /// The import tiers alone for the local name `local_name` an import
    /// binds, and the barrel a re-export hop went through
    /// ([`Self::resolve_import`]). Empty when no import tier binds it: an
    /// aliased local name (`import { render as paint }`) names no
    /// declaration, so the general tiers have nothing to look up under it.
    pub fn resolve_import_binding(
        &self,
        source_file: &Path,
        local_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
    ) -> (Vec<(NodeId, f32)>, Option<Arc<str>>) {
        let language = Language::from_normalized_path(&normalize_source_path(source_file));
        let mut barrel = None;
        let targets = self.resolve_import_tiers(
            source_file,
            language,
            local_name,
            (raw_imports, true),
            target,
            &mut barrel,
        );
        (targets, barrel)
    }

    /// The TS / JS file `import` of `importer` names
    /// ([`reexport::ecmascript_module`]), tsconfig path aliases included.
    pub(crate) fn ecmascript_module(&self, importer: &str, import: &RawImport) -> Option<Box<str>> {
        reexport::ecmascript_module(&IndexFacts(self), importer, &ImportRef::from(import))
    }

    /// The import tiers alone: the import-member tier (explicit imports, then
    /// wildcards when no explicit import binds the name) where the language
    /// binds through imports and `bind_import` holds, else the name match.
    /// `barrel` receives the barrel a re-export hop went through.
    fn resolve_import_tiers(
        &self,
        source_file: &Path,
        language: Language,
        symbol_name: &str,
        (raw_imports, bind_import): (&[RawImport], bool),
        target: ResolveTarget,
        barrel: &mut Option<Arc<str>>,
    ) -> Vec<(NodeId, f32)> {
        // `ResolveTarget::Field` reaches here only for a bare name an import
        // may bind (Java `import static app.Consts.TABLE`); an `obj.field`
        // read never takes an import tier ([`ladder::field_read`]).
        if raw_imports.is_empty() {
            return Vec::new();
        }
        if !(bind_import && import_member_fallback(language)) {
            let (targets, via) =
                self.resolve_named_import(source_file, symbol_name, raw_imports, target);
            *barrel = via;
            return targets;
        }
        match ladder::imported_member(
            &IndexFacts(self),
            &normalize_source_path(source_file),
            language,
            symbol_name,
            raw_imports,
            target,
            true,
        ) {
            ladder::MemberHit::Bound(hit) => {
                *barrel = hit.barrel;
                self.import_scoped(source_file, symbol_name, hit.import, hit.id)
                    .edge()
                    .into_iter()
                    .collect()
            }
            ladder::MemberHit::Unbound { .. } => Vec::new(),
        }
    }

    /// Tier 2 outside the import-member policy: an import whose local name
    /// is the callee, probed through the specifier's candidate files
    /// ([`reexport::named_import`]), and the barrel a re-export hop went
    /// through.
    fn resolve_named_import(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
    ) -> (Vec<(NodeId, f32)>, Option<Arc<str>>) {
        let importer = normalize_source_path(source_file);
        // Only a TS / JS caller follows a TS / JS re-export.
        let reexports = self
            .reexports
            .filter(|_| is_ecmascript(Language::from_normalized_path(&importer)));
        match reexport::named_import(
            &IndexFacts(self),
            &importer,
            raw_imports.iter().map(ImportRef::from),
            symbol_name,
            target,
            |exported_name| {
                reexports.is_some_and(|index| index.may_reach(self.symbol_table, exported_name))
            },
        ) {
            Some(hit) => (
                self.import_scoped(
                    source_file,
                    symbol_name,
                    &raw_imports[hit.position],
                    hit.decl,
                )
                .edge()
                .into_iter()
                .collect(),
                hit.barrel,
            ),
            None => (Vec::new(), None),
        }
    }

    /// Record and return an ImportScoped edge to `node_id` through `import`.
    fn import_scoped(
        &self,
        source_file: &Path,
        symbol_name: &str,
        import: &RawImport,
        node_id: NodeId,
    ) -> Resolution {
        let resolution = Resolution::bound(node_id, Tier::ImportScoped);
        self.record(
            &normalize_source_path(source_file),
            symbol_name,
            Some(import.source.as_str()),
            DecisionTier::ImportScoped,
            Some(node_id),
            0,
            resolution.edge().map(|(_, confidence)| confidence),
        );
        resolution
    }

    /// The bare-member retry of a Kotlin call through a class binding
    /// ([`ladder::extension_fallback`]): `member` through the name tiers
    /// outside a call scope, with the caller's heritage, top-level functions
    /// only.
    pub(crate) fn extension_fallback(
        &self,
        source_file: &Path,
        member: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
        caller_heritage: &[String],
    ) -> Vec<(NodeId, f32)> {
        ladder::extension_fallback(self.symbol_table, member, |member| {
            self.resolve_symbol_with_heritage(
                source_file,
                member,
                raw_imports,
                target,
                caller_heritage,
            )
        })
        .collect()
    }

    /// The C# extension retry ([`csharp_extension::resolve`]) of `member` on
    /// a receiver whose own member lookup bound nothing. The receiver's type
    /// is read where [`ReceiverSite`] says: a named type in `source_file`
    /// with `raw_imports`, the type a call returns in the callee's file with
    /// that file's imports, or none for an untyped receiver. The candidates'
    /// scope is always the caller's:
    /// `source_file`'s namespaces and `raw_imports`. `arguments` are the
    /// ones the call's entry records.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn csharp_extension(
        &self,
        source_file: &Path,
        receiver: ReceiverSite<'_>,
        member: &str,
        arguments: Option<CallArguments<'_>>,
        raw_imports: &[RawImport],
        caller_owner: Option<&str>,
    ) -> csharp_extension::Verdict {
        let st = self.symbol_table;
        let built;
        let decls = match self.csharp_extensions {
            Some(map) => map.get(member).map_or(&[][..], Vec::as_slice),
            None => {
                built = st.csharp_extension_decls(member);
                built.as_slice()
            }
        };
        if decls.is_empty() {
            return csharp_extension::Verdict::NotExtension;
        }
        let caller_file = normalize_source_path(source_file);
        let (name, scope_file, scope_imports) = match receiver {
            ReceiverSite::Named(name) => (Some(name), &*caller_file, raw_imports),
            ReceiverSite::Returned(callee) => {
                let (Some(name), Some(file)) =
                    (type_ladder::returned_type(st, callee), st.file_of(callee))
                else {
                    return csharp_extension::Verdict::NotExtension;
                };
                let imports = self
                    .reexports
                    .and_then(|index| index.modules.get(file))
                    .map_or(&[][..], |exports| exports.imports);
                (Some(name), file, imports)
            }
            ReceiverSite::Untyped => (None, &*caller_file, raw_imports),
        };
        let types: Vec<u32> = match name {
            None => Vec::new(),
            Some(name) => {
                match st.lookup_in_file_with_kind(scope_file, name, ResolveTarget::Type) {
                    Some(id) => vec![id],
                    None => type_ladder::TypeFacts::global_candidates(
                        st,
                        name,
                        ResolveTarget::Type,
                        FileMeta::from_normalized_path(scope_file),
                    )
                    .into_iter()
                    .filter(|&id| st.namespace_return_type_visible(id, scope_file, scope_imports))
                    .collect(),
                }
            }
        };
        let namespaces: Vec<&str> = st
            .csharp_namespaces(&caller_file)
            .iter()
            .map(String::as_str)
            .collect();
        csharp_extension::resolve(
            st,
            decls,
            csharp_extension::Receiver {
                name,
                types: &types,
            },
            member,
            arguments,
            &csharp_extension::CallerScope {
                file: &caller_file,
                namespaces: &namespaces,
                imports: raw_imports,
                owner: caller_owner,
            },
        )
    }

    /// Variant that exposes the caller's enclosing-class heritage to enable
    /// Tier 2.75 (`HeritageScoped`). Production call edges should prefer this
    /// so cross-file mixin / inherited-method references resolve through
    /// `Bar extends Foo` / `class Bar; include Foo; end` without falling
    /// through to the strict Global tier.
    pub fn resolve_symbol_with_heritage(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
        caller_heritage: &[String],
    ) -> Vec<(NodeId, f32)> {
        self.resolve_symbol_with_barrel(
            source_file,
            symbol_name,
            raw_imports,
            target,
            caller_heritage,
            &mut None,
        )
    }

    /// [`Self::resolve_symbol`] for the name an import statement binds, and
    /// the barrel the import tier's re-export hop went through: the module
    /// the import names, when the bound declaration lives in a module that
    /// barrel re-exports from (`import { render } from './index'` binds
    /// `impl.ts`'s `render` through `index.ts`). `post_process::imports_edges`
    /// keeps the importer's edge to that barrel.
    pub fn resolve_import(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
    ) -> (Vec<(NodeId, f32)>, Option<Arc<str>>) {
        let mut barrel = None;
        let targets = self.resolve_symbol_with_barrel(
            source_file,
            symbol_name,
            raw_imports,
            target,
            &[],
            &mut barrel,
        );
        (targets, barrel)
    }

    /// [`Self::resolve_symbol_with_heritage`]; `barrel` receives the barrel
    /// an import-tier hit went through ([`Self::resolve_import`]).
    fn resolve_symbol_with_barrel(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
        caller_heritage: &[String],
        barrel: &mut Option<Arc<str>>,
    ) -> Vec<(NodeId, f32)> {
        let language = Language::from_normalized_path(&normalize_source_path(source_file));
        // FQN call binding must not change the heritage graph underneath the
        // receiver ladder. Constructors enter through resolve_call instead.
        let bind_import = target != ResolveTarget::Type || !fqn_language(language);
        self.resolve_symbol_with_import_binding(
            source_file,
            symbol_name,
            language,
            (raw_imports, bind_import),
            target,
            caller_heritage,
            None,
            barrel,
        )
    }

    /// `call` is the call site ([`Self::resolve_call`]), `None` for a plain
    /// symbol lookup. Its presence also admits
    /// [`ResolutionTier::NearestCopy`], whose confidence Pass 2 maps to the
    /// call edge's reason; a retry under another name keeps it.
    #[allow(clippy::too_many_arguments)]
    fn resolve_symbol_with_import_binding(
        &self,
        source_file: &Path,
        symbol_name: &str,
        language: Language,
        (raw_imports, bind_import): (&[RawImport], bool),
        target: ResolveTarget,
        caller_heritage: &[String],
        call: Option<CallPick<'_>>,
        barrel: &mut Option<Arc<str>>,
    ) -> Vec<(NodeId, f32)> {
        let mut results = Vec::new();
        // Normalize path to use forward slashes to match indexed paths.
        let source_file_str = normalize_source_path(source_file);

        // Tier 1: Try SameFile (kind-aware so a property named `Foo` doesn't
        // win the lookup for a constructor call `Foo()` in the same file —
        // see `SymbolTable::file_scoped` doc). A call site narrows the pick
        // by owner; when the file holds several unrelated owners of the
        // name, Tier 1 binds nothing and the later tiers decide. A bare call
        // that reaches no member ([`CallSite::reaches_free_only`]) weighs no method of
        // the file, so a free function of the name still wins.
        let free_only = call.is_some_and(|scope| scope.free_only);
        if let Resolution::Bound(hit) = ladder::same_file(
            self.symbol_table,
            &source_file_str,
            symbol_name,
            target,
            call,
            caller_heritage,
        ) {
            results.push((hit.id, hit.confidence));
            self.record(
                &source_file_str,
                symbol_name,
                None,
                DecisionTier::SameFile,
                Some(hit.id),
                0,
                Some(hit.confidence),
            );
            return results; // Highest precedence, return early
        }

        // Tier 1 across the files of a C# partial type: a bare call in one
        // part reaches what the caller's type owns, as `this.m()` does
        // (codex ruling P-d). Only a member the type declares in any part,
        // or inherits through the bases any part lists, binds.
        if let Some((node_id, tier, decision)) = call
            .filter(|scope| !scope.untyped && !scope.free_only)
            .and_then(|scope| {
                self.partial_type_member(
                    &source_file_str,
                    language,
                    symbol_name,
                    scope.owner?,
                    target,
                )
            })
        {
            let conf = tier.confidence();
            results.push((node_id, conf));
            self.record(
                &source_file_str,
                symbol_name,
                None,
                decision,
                Some(node_id),
                0,
                Some(conf),
            );
            return results;
        }

        // Tier 2: ImportScoped.
        let legacy_type_import = target == ResolveTarget::Type && fqn_language(language);
        let has_imports = (bind_import || legacy_type_import)
            && !raw_imports.is_empty()
            && target != ResolveTarget::Field;
        let member_fallback = bind_import && has_imports && import_member_fallback(language);
        // A wildcard binds only when no explicit import binds the name, and
        // only after the caller's heritage: members in scope beat imports.
        let mut wildcard_open = false;
        if member_fallback {
            match ladder::import_member(
                &IndexFacts(self),
                &source_file_str,
                language,
                symbol_name,
                raw_imports,
                target,
                caller_heritage,
            ) {
                ladder::ImportMember::Bound(hit) => {
                    *barrel = hit.barrel;
                    return self
                        .import_scoped(source_file, symbol_name, hit.import, hit.id)
                        .edge()
                        .into_iter()
                        .collect();
                }
                ladder::ImportMember::Extension(member) => {
                    return self.extension_fallback(
                        source_file,
                        member,
                        raw_imports,
                        target,
                        caller_heritage,
                    );
                }
                ladder::ImportMember::External => {
                    self.record(
                        &source_file_str,
                        symbol_name,
                        None,
                        DecisionTier::Unresolved,
                        None,
                        0,
                        None,
                    );
                    return Vec::new();
                }
                // An import that may be local keeps the previous resolution
                // path: Python under the member's short name, a fully
                // qualified language under the callee as written.
                ladder::ImportMember::Retry(retry) => {
                    return self.resolve_symbol_with_import_binding(
                        source_file,
                        retry,
                        language,
                        (
                            &[],
                            target != ResolveTarget::Type || !fqn_language(language),
                        ),
                        target,
                        caller_heritage,
                        call,
                        &mut None,
                    );
                }
                ladder::ImportMember::Continue { wildcard } => wildcard_open = wildcard,
            }
        } else if has_imports {
            let (imported, via) =
                self.resolve_named_import(source_file, symbol_name, raw_imports, target);
            if !imported.is_empty() {
                *barrel = via;
                return imported;
            }
        }

        // Tier 2.5, then Tier 3.5 for a `::` path ([`ladder::qualified`]):
        // a callee that carries a qualifier (`A::new`, `std::vec::Vec::new`,
        // `Cls.method`) resolves through its qualifier or not at all. Tiers 1
        // and 2 are keyed by short names, and Tier 3's kind + unique filter
        // near-always rejects the common member names (`new`, `from`).
        if let Some((qualifier, member)) = split_qualifier(symbol_name) {
            let site = ladder::QualifiedSite {
                source: &source_file_str,
                caller: FileMeta::from_path(&source_file_str),
                language,
                imports: raw_imports,
                callee: symbol_name,
                qualifier,
                member,
                target,
                call,
            };
            let (resolution, base) = ladder::qualified(
                &IndexFacts(self),
                site,
                caller_heritage,
                StemFile::Any,
                || {
                    ladder::module_tree_member(
                        &self.module_tree()?,
                        self.symbol_table,
                        &source_file_str,
                        symbol_name,
                        member,
                        target,
                    )
                },
            );
            match resolution {
                Resolution::Bound(hit) => {
                    let tier = match hit.tier {
                        Some(Tier::ModuleTree) => DecisionTier::ModuleTree,
                        Some(Tier::TypeOwned) => DecisionTier::TypeOwned,
                        Some(Tier::TypeHeritage) => DecisionTier::TypeHeritage,
                        _ => DecisionTier::QualifierScoped,
                    };
                    results.push((hit.id, hit.confidence));
                    self.record(
                        &source_file_str,
                        symbol_name,
                        base,
                        tier,
                        Some(hit.id),
                        0,
                        Some(hit.confidence),
                    );
                    return results;
                }
                // A `super()` call no base owns: no later tier runs.
                Resolution::NoEdge(_) => {
                    self.record(
                        &source_file_str,
                        symbol_name,
                        None,
                        DecisionTier::Unresolved,
                        None,
                        0,
                        None,
                    );
                    return results;
                }
                Resolution::Unknown | Resolution::Shadowed => {}
            }

            // Receiver-typing ladder: runs only where every tier above found
            // nothing, so it adds edges and never moves one.
            let tier = match self.resolve_member_via_type(
                source_file,
                &source_file_str,
                symbol_name,
                qualifier,
                member,
                target,
                raw_imports,
                call.is_some(),
            ) {
                TypeMember::Edge(node_id, tier, conf) => {
                    results.push((node_id, conf));
                    self.record(
                        &source_file_str,
                        symbol_name,
                        None,
                        tier,
                        Some(node_id),
                        0,
                        Some(conf),
                    );
                    return results;
                }
                TypeMember::Ambiguous => DecisionTier::AmbiguousGlobal,
                TypeMember::NoEdge => DecisionTier::Unresolved,
            };
            let extension = match tier {
                DecisionTier::Unresolved => ladder::kotlin_extension_retry(
                    language,
                    symbol_name,
                    (qualifier, member),
                    raw_imports,
                    caller_heritage,
                ),
                _ => None,
            };
            if let Some(member) = extension {
                return self.extension_fallback(
                    source_file,
                    member,
                    raw_imports,
                    target,
                    caller_heritage,
                );
            }
            self.record(
                &source_file_str,
                symbol_name,
                None,
                tier,
                None,
                self.symbol_table.global_match_count(member),
                None,
            );
            return results;
        }

        // Tier 2.75: HeritageScoped — bare-name callee in a class that
        // extends/includes/mixes in another type. Treat each parent name as
        // an implicit qualifier and probe the parent's defining file. This
        // is what makes `class Bar; include Foo; end` resolve a delegated
        // `read` defined inside `Foo` across files, and the same path serves
        // Java/Kotlin/C# subclasses calling inherited methods without `this.`.
        // Stops at the first hit (heritage order is the source-order list
        // recorded by the parser, mirroring MRO precedence).
        // An untyped receiver is no `this`: the caller's bases do not own
        // the member it reaches. Neither do they own what a bare call that
        // reaches no member names.
        if let (Resolution::Bound(hit), Some(base)) = ladder::heritage(
            self.symbol_table,
            caller_heritage,
            symbol_name,
            target,
            call,
            |base| {
                ladder::qualifier_file(
                    &IndexFacts(self),
                    &source_file_str,
                    FileMeta::from_path(&source_file_str),
                    raw_imports,
                    (None, base, symbol_name),
                    target,
                    StemFile::Any,
                )
            },
        ) {
            results.push((hit.id, hit.confidence));
            self.record(
                &source_file_str,
                symbol_name,
                Some(base),
                DecisionTier::HeritageScoped,
                Some(hit.id),
                0,
                Some(hit.confidence),
            );
            return results;
        }

        if let Some(hit) = ladder::wildcard_member(
            &IndexFacts(self),
            wildcard_open,
            &source_file_str,
            language,
            symbol_name,
            raw_imports,
            target,
        ) {
            *barrel = hit.barrel;
            return self
                .import_scoped(source_file, symbol_name, hit.import, hit.id)
                .edge()
                .into_iter()
                .collect();
        }

        // Tier 3 ([`Self::global_tier`]).
        results.extend(
            self.global_tier(
                &source_file_str,
                symbol_name,
                raw_imports,
                target,
                call.is_some(),
                free_only,
            )
            .edge(),
        );
        results
    }

    /// R24: the `Property` a plain field read `name` binds, through the tier
    /// [`ladder::field_read`] picks: Tier 1, Tier 3 ([`Self::global_tier`]),
    /// or for a qualified read the full lookup ([`Self::resolve_symbol`]),
    /// whose qualifier tier settles it. The decision dump records each tier
    /// as the full lookup does.
    pub fn resolve_field_read(
        &self,
        source_file: &Path,
        name: &str,
        raw_imports: &[RawImport],
    ) -> Vec<(NodeId, f32)> {
        let source_file_str = normalize_source_path(source_file);
        match ladder::field_read(self.symbol_table, &source_file_str, name) {
            FieldRead::Bound(hit) => {
                self.record(
                    &source_file_str,
                    name,
                    None,
                    DecisionTier::SameFile,
                    Some(hit.id),
                    0,
                    Some(hit.confidence),
                );
                vec![(hit.id, hit.confidence)]
            }
            FieldRead::Qualified => {
                self.resolve_symbol(source_file, name, raw_imports, ResolveTarget::Field)
            }
            FieldRead::Global => self
                .global_tier(
                    &source_file_str,
                    name,
                    raw_imports,
                    ResolveTarget::Field,
                    false,
                    false,
                )
                .edge()
                .into_iter()
                .collect(),
        }
    }

    /// Tier 3: Global fallback ([`ladder::global`]) — emit only when the
    /// kind-filtered candidate set is unique. Refusing to guess on ambiguity
    /// is the dominant defence against bare-name fan-out (`new`, `format`,
    /// `default`, `main`, ...). `alt_count` in the dump still surfaces the
    /// raw same-name count so the verification harness can distinguish
    /// suppressed-ambiguous from truly-unresolved. `nearest` (a call site)
    /// admits the nearest copy; `free_only` is a bare `f()` that never
    /// reaches a member.
    fn global_tier(
        &self,
        source_file_str: &str,
        symbol_name: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
        nearest: bool,
        free_only: bool,
    ) -> Resolution {
        let specifier = raw_imports
            .iter()
            .find(|i| match &i.alias {
                Some(a) => a == symbol_name,
                None => i.imported_name == symbol_name,
            })
            .map(|i| i.source.as_str());
        let raw_count = self.symbol_table.global_match_count(symbol_name);
        let caller_meta = FileMeta::from_path(source_file_str);

        let hidden = |node_id: NodeId| {
            self.symbol_table.file_of(node_id).is_some_and(|module| {
                reexport::import_hides(
                    &IndexFacts(self),
                    source_file_str,
                    symbol_name,
                    raw_imports.iter().map(ImportRef::from),
                    module,
                )
            })
        };
        let resolution = ladder::global(
            self.symbol_table,
            source_file_str,
            caller_meta,
            target,
            nearest,
            || {
                self.symbol_table
                    .global_field_candidates(symbol_name, target, free_only)
            },
            hidden,
        );
        let (tier, target_id, confidence, alt_count) = match resolution {
            Resolution::Bound(hit) => {
                let tier = match hit.tier {
                    Some(Tier::NearestCopy) => DecisionTier::NearestCopy,
                    _ => DecisionTier::Global,
                };
                (
                    tier,
                    Some(hit.id),
                    Some(hit.confidence),
                    raw_count.saturating_sub(1),
                )
            }
            Resolution::NoEdge(NoEdge::Ambiguous(_)) => {
                (DecisionTier::AmbiguousGlobal, None, None, raw_count)
            }
            _ => (DecisionTier::Unresolved, None, None, raw_count),
        };
        self.record(
            source_file_str,
            symbol_name,
            specifier,
            tier,
            target_id,
            alt_count,
            confidence,
        );
        resolution
    }
}

/// The index ladder's answer as a [`Resolution`]. Every tier answers with
/// at most one target (B §3.4), so the first is the edge.
fn ladder_resolution(targets: &[(NodeId, f32)]) -> Resolution {
    debug_assert!(targets.len() <= 1, "a tier bound {targets:?}");
    Resolution::from_ladder(targets.first().copied())
}

/// The one target of a type lookup; `None` for none or several, which the
/// constructor fallback reads as no type.
fn sole_target(targets: &[(NodeId, f32)]) -> Option<(NodeId, f32)> {
    match *targets {
        [target] => Some(target),
        _ => None,
    }
}

/// The kinds a call site may reach. Rust method syntax (`x.f()`, recorded as
/// `T.f` or as an untyped member) calls only methods: a free `fn` is never
/// in scope through `.`. Paths use `::`, so a Rust callee holds `.` only
/// when it was written with method syntax. Python / JS `obj.f()` can reach a
/// module function, so other languages keep `Callable`.
fn call_target(source_file: &Path, site: CallSite<'_>) -> ResolveTarget {
    if site.uses_method_syntax() && is_rust_source(source_file) {
        ResolveTarget::Method
    } else {
        ResolveTarget::Callable
    }
}

/// [`reexport::for_each_specifier_candidate`] with the project's path
/// aliases.
fn for_each_specifier_candidate<F>(
    source_file: &std::path::Path,
    specifier: &str,
    aliases: &PathAliases,
    visit: F,
) where
    F: FnMut(&str) -> bool,
{
    reexport::for_each_specifier_candidate(
        source_file,
        specifier,
        |expanded| {
            if !aliases.is_empty() {
                aliases.expand(specifier, |alias| expanded(alias));
            }
        },
        visit,
    );
}

/// Outcome of the receiver-typing ladder for one qualified callee.
enum TypeMember {
    Edge(NodeId, DecisionTier, f32),
    /// Several candidate types could own the member: no edge, and the dump
    /// records `AmbiguousGlobal`.
    Ambiguous,
    NoEdge,
}

impl<'a> Resolver<'a> {
    /// Receiver-typing ladder (after Tier 3.5): resolve `qualifier.member`
    /// through the member's owning type instead of the qualifier's file.
    ///
    /// It emits an edge only when the qualifier names a project type, and
    /// that type or one of its resolved declared supertypes owns the member,
    /// and no unresolved base can shadow the owner. It never matches the
    /// member name alone.
    #[allow(clippy::too_many_arguments)]
    fn resolve_member_via_type(
        &self,
        source_file: &Path,
        source_file_str: &str,
        symbol_name: &str,
        qualifier: &str,
        member: &str,
        target: ResolveTarget,
        raw_imports: &[RawImport],
        call: bool,
    ) -> TypeMember {
        // Call targets only: the supertypes pre-pass resolves heritage with this
        // resolver before the supertypes exist, so a Type-target ladder would
        // make Pass 2 heritage edges disagree with the pre-pass. A path whose
        // prefix is an external module (`std::sync::Arc::new`) names a type
        // the project cannot own, the same guard Tier 4 applies.
        if !matches!(target, ResolveTarget::Callable | ResolveTarget::Method)
            || !qualifier_prefix_is_internal(symbol_name, qualifier)
        {
            return TypeMember::NoEdge;
        }
        let candidates = self.type_candidates(source_file, source_file_str, qualifier, raw_imports);
        match type_ladder::member_via_type(
            self.symbol_table,
            candidates,
            source_file_str,
            member,
            target,
            call,
        ) {
            LadderMember::Owned(id) => {
                TypeMember::Edge(id, DecisionTier::TypeOwned, Tier::TypeOwned.confidence())
            }
            LadderMember::Inherited(id) => TypeMember::Edge(
                id,
                DecisionTier::TypeHeritage,
                Tier::TypeHeritage.confidence(),
            ),
            LadderMember::Candidate(id) => TypeMember::Edge(
                id,
                DecisionTier::TypeCandidates,
                Tier::TypeCandidates.confidence(),
            ),
            LadderMember::NearestCopy(id) => TypeMember::Edge(
                id,
                DecisionTier::NearestCopy,
                Tier::NearestCopy.confidence(),
            ),
            LadderMember::Ambiguous => TypeMember::Ambiguous,
            LadderMember::NoEdge => TypeMember::NoEdge,
        }
    }

    /// The member a bare C# call `name()` reaches in its caller's own type,
    /// `owner`, when that type is a `partial` type of several parts
    /// (`SymbolTable::partial_parts`): the ladder from the caller's own part,
    /// which reads every part's members and bases. Only an owned or an
    /// inherited member binds; `None` for any other caller or verdict.
    fn partial_type_member(
        &self,
        source_file_str: &str,
        language: Language,
        name: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> Option<(NodeId, Tier, DecisionTier)> {
        if language != Language::CSharp
            || split_qualifier(name).is_some()
            || !matches!(target, ResolveTarget::Callable | ResolveTarget::Method)
        {
            return None;
        }
        let owner = owner_key(owner)?;
        let ty = self.symbol_table.lookup_in_file_with_kind(
            source_file_str,
            owner,
            ResolveTarget::Type,
        )?;
        if self.symbol_table.partial_parts(ty).is_empty() {
            return None;
        }
        match type_ladder::member_via_type(
            self.symbol_table,
            TypeCandidates::Unique(ty, owner),
            source_file_str,
            name,
            target,
            true,
        ) {
            LadderMember::Owned(id) => Some((id, Tier::TypeOwned, DecisionTier::TypeOwned)),
            LadderMember::Inherited(id) => {
                Some((id, Tier::TypeHeritage, DecisionTier::TypeHeritage))
            }
            _ => None,
        }
    }

    /// Ladder step 1: the project types `qualifier` can name, with no member
    /// gate. Kind filter `is_type` only: a Module / Namespace qualifier keeps
    /// the Tier 2.5 / Tier 4 path.
    ///
    /// A Rust name reaches a file only through its own definition or a `use`.
    /// With neither on record (a generic parameter `T`, or a `use` the parser
    /// does not record, such as a nested `ext::{io::Builder}`), the global
    /// candidates are a guess, so Rust takes them only behind an in-project
    /// import (a `pub use` re-export).
    fn type_candidates<'q>(
        &self,
        source_file: &Path,
        source_file_str: &str,
        qualifier: &'q str,
        raw_imports: &'q [RawImport],
    ) -> TypeCandidates<'q> {
        let st = self.symbol_table;
        if let Some(id) =
            st.lookup_in_file_with_kind(source_file_str, qualifier, ResolveTarget::Type)
        {
            return TypeCandidates::Unique(id, qualifier);
        }
        let language = Language::from_normalized_path(source_file_str);
        // A C# `using A = Ns.Type;` binds `A` to that type path. Its parse
        // also records `A` as an unaliased import, so it is read here, before
        // the import loop takes that record for a library name.
        if language == Language::CSharp {
            if let Some(path) = raw_imports.iter().find_map(|import| {
                (import.alias.as_deref() == Some(qualifier))
                    .then_some(import.imported_name.as_str())
            }) {
                return self.csharp_alias_type(source_file_str, path);
            }
        }
        let mut imported_in_project = false;
        for import in raw_imports {
            if import
                .alias
                .as_deref()
                .unwrap_or(import.imported_name.as_str())
                != qualifier
            {
                continue;
            }
            if fqn_language(language) {
                // A fully qualified import names a class path, not a file:
                // locate it as the import tier does.
                let exported = fqn_imported_name(import);
                let in_file =
                    |file: &str| st.lookup_in_file_with_kind(file, exported, ResolveTarget::Type);
                let hit = match self.import_module_file(source_file, &import.source, language) {
                    // The file declares the name under another owner, or
                    // twice: the import's type is not this one, and a
                    // global namesake would be no better.
                    Module::Unique(file) if !st.owner_path_agrees(&file, fqn_type_path(import)) => {
                        return TypeCandidates::None;
                    }
                    Module::Unique(file) => in_file(&file),
                    Module::Namespace(files) => {
                        let mut hits = files.iter().filter_map(|file| in_file(file));
                        hits.next().filter(|_| hits.next().is_none())
                    }
                    // Java and PHP have always read an import with no
                    // in-repo file as a library type (`use Exception;`), so a
                    // same-named project class elsewhere is not this receiver.
                    // Kotlin imports never matched before, so only a proven
                    // external one stops its global lookup.
                    Module::Missing
                        if language != Language::Kotlin
                            || ladder::import_is_external(
                                &IndexFacts(self),
                                &normalize_source_path(source_file),
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
            let mut hit: Option<NodeId> = None;
            self.for_each_candidate(source_file, &import.source, |cand| {
                if !st.has_file(cand) {
                    return true;
                }
                in_project = true;
                // By declared name: the ladder looks the type's members up
                // under the name this returns, so an aliased export (`export
                // { Impl as Widget }`) would need the local name carried out.
                hit = st.lookup_in_file_with_kind(cand, exported, ResolveTarget::Type);
                hit.is_none()
            });
            if let Some(id) = hit {
                return TypeCandidates::Unique(id, exported);
            }
            if !in_project {
                // A library type (`from psqlpy import Connection`): a project
                // type of the same name elsewhere is not this receiver.
                return TypeCandidates::External;
            }
            imported_in_project = true;
        }
        let caller_meta = FileMeta::from_path(source_file_str);
        if caller_meta.language == Language::Rust && !imported_in_project {
            return TypeCandidates::None;
        }
        let ids = st.global_candidates(qualifier, ResolveTarget::Type, caller_meta);
        match ids.len() {
            0 => TypeCandidates::None,
            1 => TypeCandidates::Unique(ids[0], qualifier),
            _ => TypeCandidates::Set(ids, qualifier),
        }
    }

    /// The project types a C# `using Alias = Ns.Type;` directive names: the
    /// types declared as the path's last segment whose file agrees with the
    /// whole path ([`SymbolTable::owner_path_agrees`]). A path that no
    /// project type agrees with names a library type
    /// (`using XAssert = Xunit.Assert;`), so a project `Assert` is not it.
    fn csharp_alias_type<'q>(&self, source_file_str: &str, path: &'q str) -> TypeCandidates<'q> {
        let st = self.symbol_table;
        let Some(name) = owner_key(path) else {
            return TypeCandidates::External;
        };
        let ids: Vec<NodeId> = st
            .global_candidates(
                name,
                ResolveTarget::Type,
                FileMeta::from_path(source_file_str),
            )
            .into_iter()
            .filter(|&id| {
                st.file_of(id)
                    .is_some_and(|file| st.owner_path_agrees(file, path))
            })
            .collect();
        match ids.len() {
            0 => TypeCandidates::External,
            1 => TypeCandidates::Unique(ids[0], name),
            _ => TypeCandidates::Set(ids, name),
        }
    }

    /// For a Rust path call `a::b::Q::m`, may `Q` be the type declared in
    /// `candidate_file`? The module path `a::b` names where `Q` lives, so the
    /// candidate must sit in that module: the file the module tree or the
    /// file layout places it at, or, when no file holds the module, an
    /// inline `mod` of the caller's own file. A head brought in by a `use`
    /// expands first; one that names an external crate (`use serde::de;`)
    /// places `Q` outside the project. A call with no module path before
    /// `Q`, or only `crate` / `self` / `super`, always agrees.
    fn rust_type_path_agrees(
        &self,
        source_file: &Path,
        source_file_str: &str,
        full_callee: Option<&str>,
        qualifier: &str,
        candidate_file: &str,
        imports: &[RawImport],
    ) -> bool {
        let Some(full) = full_callee else {
            return true;
        };
        let Some(prefix) = rust_type_path_prefix(full, qualifier) else {
            return true;
        };
        if qualifier_prefix_is_internal(full, qualifier) || !is_rust_source(source_file) {
            return true;
        }
        let named = rust_expand_path_head(prefix, imports);
        let declared_here = |module: &str| {
            self.symbol_table
                .lookup_in_file_with_kind(source_file_str, module, ResolveTarget::Qualifier)
                .is_some()
        };
        let module_path = match named[0] {
            "crate" | "self" | "super" => named.join("::"),
            head if declared_here(head) => format!("self::{}", named.join("::")),
            _ => {
                // Only a workspace crate's module is left; the tree places
                // `Q` itself, through its `pub use` chain.
                return self
                    .mod_tree_resolve(
                        source_file_str,
                        &format!("{}::{qualifier}", named.join("::")),
                    )
                    .is_some_and(|resolved| resolved.file == candidate_file);
            }
        };
        // The module may only re-export `Q` (`mod lock; pub use
        // lock::FileLock;`): the tree follows that `pub use` chain to the
        // defining file, which the module's own file is not.
        if let Some(resolved) = self
            .mod_tree_resolve(source_file_str, &format!("{module_path}::{qualifier}"))
            .filter(|resolved| resolved.item_name == qualifier)
        {
            return resolved.file == candidate_file;
        }
        if let Some(module_file) = self.rust_module_file(source_file, &module_path) {
            return module_file == candidate_file;
        }
        candidate_file == source_file_str
            && named
                .iter()
                .filter(|seg| !matches!(**seg, "crate" | "self" | "super"))
                .all(|&seg| declared_here(seg))
    }

    /// The indexed file of Rust module `module_path` (`crate::a::m` →
    /// `src/a/m.rs` or `src/a/m/mod.rs`), when the path is crate-, self- or
    /// super-anchored and such a file exists.
    fn rust_module_file(&self, source_file: &Path, module_path: &str) -> Option<String> {
        if let Some(file) = self.anchored_rust_module(source_file, module_path) {
            return self.symbol_table.has_file(file).then(|| file.to_string());
        }
        let base = rust_module_path_base(source_file, module_path)?;
        let base = base.to_string_lossy().replace('\\', "/");
        let mut found = None;
        probe_rust_module(base.trim_start_matches("./"), &mut |candidate: &str| {
            let indexed = self.symbol_table.has_file(candidate);
            if indexed {
                found = Some(candidate.to_string());
            }
            !indexed
        });
        found
    }

    /// Tier 3.5's facts ([`ladder::ModuleTreeFacts`]): the workspace module
    /// tree. `None` without one (no `Cargo.toml`, a non-Rust repo).
    fn module_tree(&self) -> Option<IndexModuleTree<'_>> {
        Some(self.mod_tree?.facts(self.workspace_root.as_deref()?))
    }

    fn mod_tree_resolve(
        &self,
        source_file_str: &str,
        fqn: &str,
    ) -> Option<crate::rust::module_tree::ResolvedFqn> {
        self.mod_tree?
            .resolve_fqn(fqn, source_file_str, self.workspace_root.as_ref()?)
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        src_file: &str,
        name: &str,
        specifier: Option<&str>,
        tier: DecisionTier,
        target_id: Option<NodeId>,
        alt_count: u32,
        confidence: Option<f32>,
    ) {
        // Production path: `self.decisions` is `None` → single
        // Option-discriminant branch and we're out. No Mutex touch.
        let Some(cell) = self.decisions.as_ref() else {
            return;
        };
        let mut guard = cell.lock().expect("resolver dump mutex poisoned");
        guard.push(ResolverDecision {
            src_file: src_file.to_string(),
            name: name.to_string(),
            specifier: specifier.map(|s| s.to_string()),
            tier,
            target_id,
            alt_count,
            confidence,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolution::heuristics::ResolutionTier;
    use ecp_core::analyzer::rust_paths::rust_path_prefix_agrees_with_import;
    use std::path::PathBuf;

    fn cands(src: &str, spec: &str) -> Vec<String> {
        let mut out = Vec::new();
        let aliases = PathAliases::new();
        for_each_specifier_candidate(&PathBuf::from(src), spec, &aliases, |c| {
            out.push(c.to_string());
            true
        });
        out
    }

    #[test]
    fn verbatim_specifier_is_always_first_candidate() {
        let c = cands("src/a/b.ts", "./foo");
        assert_eq!(c[0], "./foo", "verbatim must lead the candidate list");
    }

    #[test]
    fn ts_dot_relative_resolves_against_source_dir_with_ext_and_index() {
        let c = cands("src/a/b.ts", "./foo");
        assert!(
            c.contains(&"src/a/foo.ts".to_string()),
            "should include src/a/foo.ts: {c:?}"
        );
        assert!(
            c.contains(&"src/a/foo/index.ts".to_string()),
            "should include src/a/foo/index.ts: {c:?}"
        );
    }

    #[test]
    fn ts_parent_relative_walks_up_one_dir() {
        let c = cands("src/a/b.ts", "../helpers/util");
        assert!(
            c.contains(&"src/helpers/util.ts".to_string()),
            "should include src/helpers/util.ts: {c:?}"
        );
    }

    #[test]
    fn python_single_dot_resolves_to_current_package() {
        let c = cands("src/flask/__init__.py", ".globals");
        assert!(
            c.contains(&"src/flask/globals.py".to_string()),
            "should include src/flask/globals.py: {c:?}"
        );
    }

    #[test]
    fn python_dotted_submodule_replaces_dots_with_slashes() {
        let c = cands("src/pkg/x.py", ".sub.mod");
        assert!(
            c.contains(&"src/pkg/sub/mod.py".to_string()),
            "should include src/pkg/sub/mod.py: {c:?}"
        );
    }

    #[test]
    fn python_double_dot_walks_one_parent_then_drills() {
        let c = cands("src/pkg/inner/x.py", "..helpers.util");
        assert!(
            c.contains(&"src/pkg/helpers/util.py".to_string()),
            "should include src/pkg/helpers/util.py: {c:?}"
        );
    }

    /// Regression: `...foo` was generating `/foo` because `rest = "/foo"`
    /// would slip through to `Path::join("/foo")`, which on Unix discards
    /// the base. PEP 328: three dots = walk two parents.
    #[test]
    fn python_triple_dot_walks_two_parents() {
        let c = cands("src/a/b/c/d.py", "...mod");
        assert!(
            c.contains(&"src/a/mod.py".to_string()),
            "should include src/a/mod.py (walked two parents): {c:?}"
        );
        // No /-rooted entries from the dotted base — would mean the bug
        // came back.
        assert!(
            !c.iter().any(|s| s.starts_with("/mod") || s == "/"),
            "no absolute-rooted candidate should leak: {c:?}"
        );
    }

    #[test]
    fn bare_pathlike_specifier_still_emits_extension_probes() {
        let c = cands("any.ts", "components/Button");
        assert!(
            c.contains(&"components/Button.tsx".to_string())
                || c.contains(&"components/Button.ts".to_string()),
            "bare specifier should still trigger ext probes: {c:?}"
        );
    }

    #[test]
    fn package_style_index_suffix_is_offered() {
        let c = cands("src/a/b.ts", "./foo");
        assert!(
            c.iter().any(|s| s.ends_with("/index.tsx")),
            "should include some /index.tsx candidate: {c:?}"
        );
    }

    // ── Tier-3 cap (kind-filtered + unique-only) ────────────────────────────

    use ecp_core::graph::NodeKind;

    /// Build a SymbolTable from `(file, name, kind)` triples — ids auto-assigned
    /// monotonically (matching the dense-id invariant `register_node` enforces).
    fn st_with(nodes: &[(&str, &str, NodeKind)]) -> SymbolTable {
        let mut st = SymbolTable::new();
        for (id, (file, name, kind)) in nodes.iter().enumerate() {
            st.register_node(file, name, id as u32, *kind);
        }
        st
    }

    #[test]
    fn tier3_ambiguous_callable_emits_no_edge() {
        // Two same-named methods in different files → ambiguous bare call
        // refuses to guess. Pins the dominant defence against fan-out
        // (common names like `new`/`format`/`default`/`main`).
        let st = st_with(&[
            ("a.rs", "new", NodeKind::Method),
            ("b.rs", "new", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(&PathBuf::from("c.rs"), "new", &[], ResolveTarget::Callable);
        assert!(
            out.is_empty(),
            "ambiguous bare callable must not emit, got {:?}",
            out
        );
    }

    #[test]
    fn tier3_unique_callable_emits_one_edge() {
        // Single global match → still emit. The cap is about ambiguity,
        // not killing all cross-file resolution.
        let st = st_with(&[("a.rs", "process_request", NodeKind::Function)]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("c.rs"),
            "process_request",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![(0, ResolutionTier::Global.base_confidence())]);
    }

    #[test]
    fn tier3_kind_filter_excludes_non_callable() {
        // One Function + one Variable share the name. Callable target sees
        // only the Function → uniqueness restored → edge emitted. Without
        // the kind filter, both would match → ambiguous → no edge.
        let st = st_with(&[
            ("a.rs", "config", NodeKind::Function),
            ("b.rs", "config", NodeKind::Variable),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("c.rs"),
            "config",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![(0, ResolutionTier::Global.base_confidence())]);
    }

    #[test]
    fn tier1_same_file_kind_filters_out_non_callable() {
        // SameFile is now kind-aware: a Variable named `helper` in the same
        // file no longer "wins" for a Callable target — it would yield the
        // semantically-nonsense `Calls -> Variable` edge that PR #71 round-3
        // set out to remove. Falls through to Tier-3 Global, which picks
        // up the Function in b.rs (unique under the Callable predicate).
        let st = st_with(&[
            ("a.rs", "helper", NodeKind::Variable),
            ("b.rs", "helper", NodeKind::Function),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("a.rs"),
            "helper",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![(1, ResolutionTier::Global.base_confidence())]);
    }

    // ── Layer-1 barriers (language + vendor) ────────────────────────────────

    #[test]
    fn tier3_language_barrier_blocks_cross_language() {
        // Rust caller's bare `is_some` must not resolve to a uniquely-named
        // Move function. Pins against the residual fan-out where a Rust
        // `result.is_some()` was wrongly connecting to a vendor `.move` test
        // fixture's `is_some` definition.
        let st = st_with(&[("lib/option.move", "is_some", NodeKind::Function)]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("src/caller.rs"),
            "is_some",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "rust caller must not cross language boundary to move target, got {:?}",
            out
        );
    }

    #[test]
    fn tier3_vendor_barrier_blocks_source_caller() {
        // A unique callable defined under `/vendor/` is invisible to a
        // non-vendor caller. Pins vendor test corpora away from production
        // resolution surface even when language and uniqueness match.
        let st = st_with(&[(
            "crates/vendor/tree-sitter-x/tests/helper.rs",
            "uniquely_named_helper",
            NodeKind::Function,
        )]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("crates/ecp-cli/src/main.rs"),
            "uniquely_named_helper",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "non-vendor caller must not reach vendor target, got {:?}",
            out
        );
    }

    #[test]
    fn tier3_intra_vendor_resolution_preserved() {
        // Vendor → vendor calls remain resolvable. The barrier is asymmetric
        // by design (source ↛ vendor, but vendor ↔ vendor is fine for the
        // vendor crate's internal cohesion).
        let st = st_with(&[(
            "crates/vendor/tree-sitter-x/src/helper.rs",
            "vendor_helper",
            NodeKind::Function,
        )]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("crates/vendor/tree-sitter-x/src/caller.rs"),
            "vendor_helper",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(0, ResolutionTier::Global.base_confidence())],
            "intra-vendor resolution must still emit, got {:?}",
            out
        );
    }

    // ── split_qualifier ─────────────────────────────────────────────────────

    #[test]
    fn split_qualifier_handles_simple_double_colon() {
        assert_eq!(split_qualifier("A::new"), Some(("A", "new")));
    }

    #[test]
    fn split_qualifier_takes_last_segment_for_multi_path() {
        // `std::vec::Vec::new` — Vec is the immediate qualifier; `std::vec`
        // is a path prefix that the symbol table can't map back to a single
        // registered Type name.
        assert_eq!(split_qualifier("std::vec::Vec::new"), Some(("Vec", "new")));
    }

    #[test]
    fn split_qualifier_handles_dot_separator() {
        assert_eq!(split_qualifier("obj.method"), Some(("obj", "method")));
    }

    #[test]
    fn split_qualifier_returns_none_for_bare_name() {
        assert_eq!(split_qualifier("foo"), None);
    }

    #[test]
    fn split_qualifier_rejects_empty_sides() {
        assert_eq!(split_qualifier("::foo"), None);
        assert_eq!(split_qualifier("foo::"), None);
        assert_eq!(split_qualifier(".foo"), None);
        assert_eq!(split_qualifier("foo."), None);
    }

    // ── Tier 2.5: qualifier-scoped resolution ───────────────────────────────

    #[test]
    fn tier2_5_resolves_via_same_file_qualifier() {
        // `A` defined in caller's file, `new` defined in A's file (`a.rs`).
        // Caller `c.rs` invokes `A::new` — Tier 2.5 should:
        //   1. Resolve `A` as Type via Tier 1/2/3 → finds A's file (a.rs)
        //   2. Lookup `new` in a.rs → finds it
        //   3. Emit edge at QualifierScoped confidence (0.85)
        // Stands for `impl A { fn new() }`: the build registers `new` owned
        // by `A`, and a Rust `::` path binds only a method its qualifier
        // owns (`ladder::rust_path_may_bind`).
        let mut st = SymbolTable::new();
        st.register_node("a.rs", "A", 0, NodeKind::Class);
        st.register_node_owned("a.rs", "new", 1, NodeKind::Method, "A");
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("a.rs"),
            "A::new",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
    }

    #[test]
    fn tier2_5_resolves_via_global_qualifier() {
        // `A` defined in `a.rs`, caller in different file. Qualifier resolves
        // via Tier 3 (kind-filtered, unique Type), member then found in A's
        // file. This is the dominant Rust pattern (`A::new()` from another
        // module).
        // Stands for `impl A { fn new() }`: the build registers `new` owned
        // by `A`, and a Rust `::` path binds only a method its qualifier
        // owns (`ladder::rust_path_may_bind`).
        let mut st = SymbolTable::new();
        st.register_node("a.rs", "A", 0, NodeKind::Class);
        st.register_node_owned("a.rs", "new", 1, NodeKind::Method, "A");
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::new",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
    }

    #[test]
    fn tier2_5_unknown_qualifier_emits_nothing() {
        // No `A` registered as a Type anywhere. Tier 2.5 must NOT fall back
        // to bare-name `new` Tier-3: dogfood verification (B.1 dump,
        // 27.8k decisions) showed a ~52% false-positive rate on that
        // fallback path — `std::fs::read` resolving to a local `read()`,
        // `serde_json::json!` macro resolving to a local `json()`, etc.
        // The proper recovery for legitimate module-qualified free functions
        // (`registry::sanitize_branch`) is Phase B.3 (workspace-crate-aware
        // import resolution), not bare-name fallback.
        let st = st_with(&[("a.rs", "new", NodeKind::Method)]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::new",
            &[],
            ResolveTarget::Callable,
        );
        assert!(out.is_empty(), "unknown qualifier must not emit: {:?}", out);
    }

    #[test]
    fn tier2_5_member_missing_in_qualifier_file_emits_nothing() {
        // Qualifier `A` resolves to `a.rs`, but `a.rs` doesn't define
        // `nonexistent`. Member missing → no edge (no Tier-3 fallback for
        // short name even though it might be unique globally elsewhere).
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("b.rs", "nonexistent", NodeKind::Function),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::nonexistent",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "member missing in qualifier's file must not emit: {:?}",
            out
        );
    }

    #[test]
    fn tier2_5_ambiguous_qualifier_emits_nothing() {
        // Two Types named `A` in different files. The unique-only constraint
        // on the qualifier's Tier-3 step rejects ambiguity → no edge. Member
        // existing globally is irrelevant — qualified callees never degrade
        // to bare-name Tier 3 (see `tier2_5_unknown_qualifier_emits_nothing`
        // for the rationale).
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("b.rs", "A", NodeKind::Class),
            ("a.rs", "new", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::new",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "ambiguous qualifier must not emit: {:?}",
            out
        );
    }

    #[test]
    fn tier2_5_does_not_fall_back_to_tier3_for_qualified_callee() {
        // The member `unique_method` is globally unique AND would resolve via
        // bare-name Tier 3 if reached. But the callee is qualified
        // `Unknown::unique_method` and `Unknown` doesn't resolve → no edge.
        // Pins the no-guess policy: a qualified callee resolves via its
        // qualifier or not at all. Dump verification confirmed that allowing
        // this fallback restores pre-B.1 false positives (`std::fs::read` →
        // local `read()`, etc.) at a higher rate than it recovers legitimate
        // edges — those should come from Phase B.3 instead.
        let st = st_with(&[("a.rs", "unique_method", NodeKind::Function)]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "Unknown::unique_method",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "qualified callee with unresolved qualifier must not fall through to Tier-3: {:?}",
            out
        );
    }

    #[test]
    fn tier2_5_handles_multi_segment_qualifier_via_last_segment() {
        // `ns::sub::Vec::make` — qualifier folds to last segment `Vec`,
        // which resolves uniquely to `vec.cpp`, where `make` lives.
        let st = st_with(&[
            ("vec.cpp", "Vec", NodeKind::Class),
            ("vec.cpp", "make", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.cpp"),
            "ns::sub::Vec::make",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
    }

    #[test]
    fn tier2_5_rust_external_module_path_never_binds_a_project_type() {
        // Rust spells the module: `std::vec::Vec` is std's, not the
        // project's only `Vec` (FU-2026-10-03-96ccd9c59f1e).
        let st = st_with(&[
            ("vec.rs", "Vec", NodeKind::Class),
            ("vec.rs", "new", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "std::vec::Vec::new",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![]);
    }

    #[test]
    fn tier2_5_resolves_via_import() {
        // TS-style: `import { MyClass } from "./x"` then `MyClass.foo()`.
        // Tier 2.5 should resolve the qualifier via the import (Tier 2) →
        // find foo in x.ts. Confirms the import path works for the qualifier
        // resolution sub-step, not just same-file / global.
        use ecp_core::analyzer::types::RawImport;
        let st = st_with(&[
            ("src/x.ts", "MyClass", NodeKind::Class),
            ("src/x.ts", "foo", NodeKind::Method),
        ]);
        let r = Resolver::new(&st);
        let imports = vec![RawImport {
            source: "./x".to_string(),
            imported_name: "MyClass".to_string(),
            alias: None,
            binding_kind: None,
        }];
        let out = r.resolve_symbol(
            &PathBuf::from("src/caller.ts"),
            "MyClass.foo",
            &imports,
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
    }

    #[test]
    fn tier2_rust_crate_path_import_disambiguates_ambiguous_callable() {
        // The #100-122 incident root cause: a Rust bare call `emit(...)` whose
        // `use crate::output::{emit}` import names the exact source module.
        // With several same-named `emit` definitions across the workspace, the
        // Tier-3 ambiguity cap suppresses the edge — UNLESS Tier 2 expands the
        // Rust module path `crate::output` to the caller crate's
        // `src/output.rs` and resolves there first. Without the crate:: path
        // expansion, `ecp impact` undercounts callers of any common-named
        // cross-module Rust function.
        use ecp_core::analyzer::types::RawImport;
        let st = st_with(&[
            ("crates/ecp-cli/src/output.rs", "emit", NodeKind::Function),
            (
                "crates/ecp-cli/src/commands/diff/output.rs",
                "emit",
                NodeKind::Function,
            ),
            (
                "crates/ecp-analyzer/src/javascript/path_literals.rs",
                "emit",
                NodeKind::Function,
            ),
        ]);
        let r = Resolver::new(&st);
        let imports = vec![RawImport {
            source: "crate::output".to_string(),
            imported_name: "emit".to_string(),
            alias: None,
            binding_kind: None,
        }];
        let out = r.resolve_symbol(
            &PathBuf::from("crates/ecp-cli/src/commands/find.rs"),
            "emit",
            &imports,
            ResolveTarget::Callable,
        );
        assert_eq!(
            out,
            vec![(0, ResolutionTier::ImportScoped.base_confidence())],
            "crate::output import must resolve `emit` to crates/ecp-cli/src/output.rs (node 0), \
             not be suppressed by the same-name ambiguity cap"
        );
    }

    #[test]
    fn rust_module_path_expansion_does_not_touch_non_rust_callers() {
        // Generality guard: the crate::/self::/super:: expansion is gated on a
        // `.rs` caller extension. A non-Rust caller (e.g. a Move file) with a
        // `crate::`-looking specifier must NOT be expanded — its own language's
        // resolution path stays authoritative, and a same-name ambiguity is
        // still correctly suppressed rather than mis-resolved.
        assert!(
            rust_module_path_base(&PathBuf::from("src/x.ts"), "crate::output").is_none(),
            "non-.rs caller must not trigger Rust module-path expansion"
        );
        assert!(
            rust_module_path_base(&PathBuf::from("lib/m.move"), "self::a").is_none(),
            "non-.rs caller must not trigger self:: expansion"
        );
        // External crate path from a Rust caller still expands (harmless): it
        // produces a base that no SymbolTable key matches, so resolution falls
        // through exactly as before — verified by it returning Some(path) here
        // but the std symbol never being indexed.
        assert!(
            rust_module_path_base(&PathBuf::from("crates/c/src/a.rs"), "std::fs").is_none(),
            "std:: (non crate/self/super head) must not expand"
        );
    }

    #[test]
    fn test_rust_module_path_base_super_from_non_mod_file_is_own_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("a/b.rs", "super"), Some(PathBuf::from("a")));
        assert_eq!(
            base("src/commands/impact/symbol.rs", "super::x"),
            Some(PathBuf::from("src/commands/impact/x"))
        );
        assert_eq!(base("src/x.rs", "super"), Some(PathBuf::from("src")));
    }

    #[test]
    fn test_rust_module_path_base_super_from_mod_rs_is_parent_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("a/b/mod.rs", "super"), Some(PathBuf::from("a")));
        assert_eq!(base("a/b/mod.rs", "super::x"), Some(PathBuf::from("a/x")));
    }

    #[test]
    fn test_rust_module_path_base_self_from_non_mod_file_is_file_stem_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("a/b.rs", "self"), Some(PathBuf::from("a/b")));
        assert_eq!(base("a/b.rs", "self::x"), Some(PathBuf::from("a/b/x")));
    }

    #[test]
    fn test_rust_module_path_base_self_from_mod_rs_is_own_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("a/b/mod.rs", "self"), Some(PathBuf::from("a/b")));
        assert_eq!(base("a/b/mod.rs", "self::x"), Some(PathBuf::from("a/b/x")));
    }

    #[test]
    fn test_rust_module_path_base_crate_roots_keep_parent_directory_semantics() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(
            base("c/src/lib.rs", "self::x"),
            Some(PathBuf::from("c/src/x"))
        );
        assert_eq!(
            base("c/src/main.rs", "self::x"),
            Some(PathBuf::from("c/src/x"))
        );
        assert_eq!(base("c/src/lib.rs", "super"), Some(PathBuf::from("c")));
        assert_eq!(base("c/src/main.rs", "super"), Some(PathBuf::from("c")));
        assert_eq!(
            base("c/src/a/b.rs", "crate::m"),
            Some(PathBuf::from("c/src/m"))
        );
    }

    #[test]
    fn test_rust_module_path_base_super_chain_walks_one_module_per_super() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        // a::b::c  ->  super = a::b (dir a/b), super::super = a (dir a)
        assert_eq!(base("a/b/c.rs", "super::super"), Some(PathBuf::from("a")));
        assert_eq!(
            base("a/b/c.rs", "super::super::x"),
            Some(PathBuf::from("a/x"))
        );
        // a::b (mod.rs)  ->  super = a, super::super = parent of a
        assert_eq!(
            base("r/a/b/mod.rs", "super::super"),
            Some(PathBuf::from("r"))
        );
    }

    #[test]
    fn test_rust_module_path_base_self_from_cargo_target_roots_is_own_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(
            base("src/bin/tool.rs", "self"),
            Some(PathBuf::from("src/bin"))
        );
        assert_eq!(base("tests/it.rs", "self"), Some(PathBuf::from("tests")));
        assert_eq!(
            base("crates/foo/examples/demo.rs", "self"),
            Some(PathBuf::from("crates/foo/examples"))
        );
        assert_eq!(
            base("crates/foo/benches/b.rs", "self::x"),
            Some(PathBuf::from("crates/foo/benches/x"))
        );
        assert_eq!(base("build.rs", "self"), Some(PathBuf::from("")));
        assert_eq!(
            base("crates/foo/build.rs", "self::x"),
            Some(PathBuf::from("crates/foo/x"))
        );
    }

    #[test]
    fn test_rust_module_path_base_tests_dir_under_src_stays_ordinary_module() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(
            base("src/a/tests/x.rs", "self"),
            Some(PathBuf::from("src/a/tests/x"))
        );
        assert_eq!(
            base("src/a/tests/x.rs", "super"),
            Some(PathBuf::from("src/a/tests"))
        );
        assert_eq!(
            base("src/build.rs", "self"),
            Some(PathBuf::from("src/build"))
        );
        assert_eq!(base("bin/tool.rs", "self"), Some(PathBuf::from("bin/tool")));
    }

    #[test]
    fn test_rust_module_path_base_super_from_cargo_target_root_is_parent_directory() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("src/bin/tool.rs", "super"), Some(PathBuf::from("src")));
        assert_eq!(
            base("crates/foo/tests/it.rs", "super::x"),
            Some(PathBuf::from("crates/foo/x"))
        );
    }

    #[test]
    fn test_rust_module_path_base_crate_from_repo_root_crate_is_src() {
        let base = |f: &str, spec: &str| rust_module_path_base(&PathBuf::from(f), spec);
        assert_eq!(base("src/app.rs", "crate::a"), Some(PathBuf::from("src/a")));
        assert_eq!(
            base("src/lib.rs", "crate::a::b"),
            Some(PathBuf::from("src/a/b"))
        );
        assert_eq!(
            base("crates/x/src/app.rs", "crate::a"),
            Some(PathBuf::from("crates/x/src/a"))
        );
        assert_eq!(base("mysrc/app.rs", "crate::a"), None);
        assert_eq!(base("tests/it.rs", "crate::a"), None);
        assert_eq!(
            base("src\\app.rs", "crate::a"),
            Some(PathBuf::from("src/a"))
        );
        assert_eq!(
            base("crates\\x\\src\\app.rs", "crate::a"),
            Some(PathBuf::from("crates/x/src/a"))
        );
    }

    #[test]
    fn test_rust_path_prefix_agrees_with_import_cases() {
        let agrees = |callee: &str, q: &str, source: &str| {
            rust_path_prefix_agrees_with_import(callee, q, source, &[])
        };
        // No module path before the qualifier: nothing to disagree with.
        assert!(agrees("Error::custom", "Error", "crate::error"));
        assert!(agrees("custom", "Error", "crate::error"));
        assert!(agrees("err.custom", "err", "crate::error"));
        assert!(agrees("self::Error::custom", "Error", "crate::error"));
        assert!(agrees("::Error::custom", "Error", "crate::error"));
        // The named module path is a suffix of the import source.
        assert!(agrees(
            "crate::error::Error::custom",
            "Error",
            "crate::error"
        ));
        // A crate-rooted path names one module, not any module ending in it.
        assert!(!agrees(
            "crate::error::Error::custom",
            "Error",
            "crate::other::error"
        ));
        assert!(agrees("error::Error::custom", "Error", "crate::error"));
        assert!(agrees(
            "super::error::Error::custom",
            "Error",
            "crate::error"
        ));
        // Another module names another item.
        assert!(!agrees("de::Error::custom", "Error", "crate::error"));
        assert!(!agrees("crate::de::Error::custom", "Error", "crate::error"));
        assert!(!agrees("a::error::Error::custom", "Error", "crate::error"));
        // A module alias expands before the comparison, in both forms the
        // parser records.
        let alias = |source: &str, name: &str| RawImport {
            source: source.to_string(),
            imported_name: name.to_string(),
            alias: Some("err".to_string()),
            binding_kind: None,
        };
        for module in [
            alias("crate::error", "crate::error"),
            alias("crate", "error"),
        ] {
            let imports = [module];
            assert!(rust_path_prefix_agrees_with_import(
                "err::Error::custom",
                "Error",
                "crate::error",
                &imports
            ));
            assert!(!rust_path_prefix_agrees_with_import(
                "err::Error::custom",
                "Error",
                "crate::other",
                &imports
            ));
        }
        // The qualifier is not the segment before the member: not this rule.
        assert!(agrees("de::Error::custom", "de", "crate::error"));
    }

    #[test]
    fn test_rust_module_path_base_non_rust_file_with_super_is_untouched() {
        assert!(rust_module_path_base(&PathBuf::from("a/b.py"), "super::x").is_none());
        assert!(rust_module_path_base(&PathBuf::from("a/b.ts"), "self::x").is_none());
    }

    #[test]
    fn tier2_5_member_kind_filtered_inside_qualifier_file() {
        // PR #71 round-3 flipped Tier 2.5 to kind-aware lookup (the
        // previous "prefer recall" stance was producing `Calls -> Const`
        // and `Calls -> Variable` edges that have no operational meaning).
        // `A::FLAG` requesting Callable no longer resolves to the Const —
        // it falls through to Unresolved (Tier 3 Global filters on
        // Callable too, and no Callable named FLAG exists).
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("a.rs", "FLAG", NodeKind::Const),
        ]);
        let r = Resolver::new(&st);
        let out = r.resolve_symbol(
            &PathBuf::from("caller.rs"),
            "A::FLAG",
            &[],
            ResolveTarget::Callable,
        );
        assert!(
            out.is_empty(),
            "FLAG is a Const, must not surface as a Callable target; got {:?}",
            out
        );
    }

    // ── Ambiguity sentinel: distinguish "no candidates" from "many" ─────────

    #[test]
    fn tier3_ambiguous_records_ambiguous_global_decision_across_14_langs() {
        // For every mainstream language, two same-name global functions
        // produce zero edges (preserved behavior) AND a single
        // `AmbiguousGlobal` decision — replacing the prior `Unresolved`
        // outcome that conflated "not found" with "found ≥2, suppressed".
        let langs: &[&str] = &[
            "ts", "js", "py", "java", "kt", "cs", "go", "rs", "php", "rb", "swift", "c", "cpp",
            "dart",
        ];
        for ext in langs {
            let st = st_with(&[
                (
                    Box::leak(format!("a.{ext}").into_boxed_str()),
                    "ambiguity_demo",
                    NodeKind::Function,
                ),
                (
                    Box::leak(format!("b.{ext}").into_boxed_str()),
                    "ambiguity_demo",
                    NodeKind::Function,
                ),
            ]);
            let mut r = Resolver::new(&st);
            r.enable_dump();
            let out = r.resolve_symbol(
                &PathBuf::from(format!("c.{ext}")),
                "ambiguity_demo",
                &[],
                ResolveTarget::Callable,
            );
            assert!(out.is_empty(), "{ext}: ambiguous bare call must not emit");

            let decisions = r.take_decisions().unwrap();
            let last = decisions.last().expect("a decision was recorded");
            assert_eq!(
                last.tier,
                DecisionTier::AmbiguousGlobal,
                "{ext}: Tier-3 with ≥2 kind-filtered candidates must record \
                 AmbiguousGlobal, got {:?} (alt_count={})",
                last.tier,
                last.alt_count
            );
            assert!(
                last.alt_count >= 2,
                "{ext}: alt_count should reflect the candidate set, got {}",
                last.alt_count
            );
            assert!(last.target_id.is_none(), "{ext}: must not pick a target");
        }
    }

    #[test]
    fn tier3_zero_candidates_still_records_unresolved() {
        // Empty symbol table → bare name truly has no matches → keep the
        // `Unresolved` decision (NOT AmbiguousGlobal). Pins that the new
        // variant only fires on the "found, suppressed" path.
        let st = st_with(&[]);
        let mut r = Resolver::new(&st);
        r.enable_dump();
        let out = r.resolve_symbol(
            &PathBuf::from("a.rs"),
            "nonexistent",
            &[],
            ResolveTarget::Callable,
        );
        assert!(out.is_empty());

        let last = r.take_decisions().unwrap().pop().unwrap();
        assert_eq!(last.tier, DecisionTier::Unresolved);
        assert_eq!(last.alt_count, 0);
    }

    #[test]
    fn tier3_unique_kind_filter_recovers_does_not_surface_ambiguity() {
        // One Function + one Variable share the name. Callable target filters
        // out the Variable → uniqueness restored → Global decision wins.
        // Pins that AmbiguousGlobal only fires when the *post-filter* set is
        // ≥2, not when the raw same-name set is ≥2.
        let st = st_with(&[
            ("a.rs", "config", NodeKind::Function),
            ("b.rs", "config", NodeKind::Variable),
        ]);
        let mut r = Resolver::new(&st);
        r.enable_dump();
        let out = r.resolve_symbol(
            &PathBuf::from("c.rs"),
            "config",
            &[],
            ResolveTarget::Callable,
        );
        assert_eq!(out, vec![(0, ResolutionTier::Global.base_confidence())]);

        let last = r.take_decisions().unwrap().pop().unwrap();
        assert_eq!(last.tier, DecisionTier::Global);
    }

    // ── Test doubles ─────────────────────────────────────────────────────────

    #[test]
    fn tier3_test_double_keeps_production_name_ambiguous_across_14_langs() {
        // Pins the measured decision: a test fake sharing a production
        // method's name keeps every bare call to it AmbiguousGlobal. A
        // production-preferring tie-break wired driver `conn.execute` calls
        // to a project `execute`; receiver typing is the fix for those.
        let langs: &[&str] = &[
            "ts", "js", "py", "java", "kt", "cs", "go", "rs", "php", "rb", "swift", "c", "cpp",
            "dart",
        ];
        for ext in langs {
            let st = st_with(&[
                (
                    Box::leak(format!("src/service.{ext}").into_boxed_str()),
                    "scan_range",
                    NodeKind::Function,
                ),
                (
                    Box::leak(format!("tests/fakes.{ext}").into_boxed_str()),
                    "scan_range",
                    NodeKind::Function,
                ),
            ]);
            let mut r = Resolver::new(&st);
            r.enable_dump();
            let out = r.resolve_symbol(
                &PathBuf::from(format!("src/search.{ext}")),
                "scan_range",
                &[],
                ResolveTarget::Callable,
            );
            assert!(out.is_empty(), "{ext}: got {out:?}");
            let last = r.take_decisions().unwrap().pop().unwrap();
            assert_eq!(last.tier, DecisionTier::AmbiguousGlobal, "{ext}");
        }
    }

    // ── Receiver-typing ladder ──────────────────────────────────────────────

    /// `(file, name, kind, owner)` rows; ids are the row positions.
    fn st_owned(rows: &[(&str, &str, NodeKind, Option<&str>)]) -> SymbolTable {
        let mut st = SymbolTable::new();
        for (id, &(file, name, kind, owner)) in rows.iter().enumerate() {
            match owner {
                Some(o) => st.register_node_owned(file, name, id as u32, kind, o),
                None => st.register_node(file, name, id as u32, kind),
            }
        }
        st
    }

    /// Resolve a Callable `callee` from `caller`; return the edges and the
    /// tier of the last recorded decision.
    fn resolve_dumped(
        st: &SymbolTable,
        caller: &str,
        callee: &str,
        imports: &[RawImport],
    ) -> (Vec<(NodeId, f32)>, DecisionTier) {
        let mut r = Resolver::new(st);
        r.enable_dump();
        let out = r.resolve_symbol(
            &PathBuf::from(caller),
            callee,
            imports,
            ResolveTarget::Callable,
        );
        let last = r.take_decisions().unwrap().pop().expect("a decision");
        (out, last.tier)
    }

    fn import(source: &str, name: &str, alias: Option<&str>) -> RawImport {
        RawImport {
            source: source.to_string(),
            imported_name: name.to_string(),
            alias: alias.map(str::to_string),
            binding_kind: None,
        }
    }

    const HERITAGE: f32 = 0.8;

    /// `pkg/base.py: class Base: def greet`, `pkg/derived.py: class Derived`,
    /// and a decoy `greet` on an unrelated type. Ids: Base 0, Base.greet 1,
    /// Derived 2, Decoy 3, Decoy.greet 4.
    fn inherited_greet() -> SymbolTable {
        st_owned(&[
            ("pkg/base.py", "Base", NodeKind::Class, None),
            ("pkg/base.py", "greet", NodeKind::Method, Some("Base")),
            ("pkg/derived.py", "Derived", NodeKind::Class, None),
            ("pkg/decoy.py", "Decoy", NodeKind::Class, None),
            ("pkg/decoy.py", "greet", NodeKind::Method, Some("Decoy")),
        ])
    }

    #[test]
    fn test_resolve_member_via_type_member_in_sibling_impl_file_returns_type_owned() {
        let st = st_owned(&[
            ("src/model.rs", "Repo", NodeKind::Struct, None),
            ("src/repo_impl.rs", "save", NodeKind::Method, Some("Repo")),
            ("src/decoy.rs", "Decoy", NodeKind::Struct, None),
            ("src/decoy.rs", "save", NodeKind::Method, Some("Decoy")),
        ]);
        let imports = [import("crate::model", "Repo", None)];
        let (out, tier) = resolve_dumped(&st, "src/app.rs", "Repo.save", &imports);
        assert_eq!(
            out,
            vec![(1, ResolutionTier::QualifierScoped.base_confidence())]
        );
        assert_eq!(tier, DecisionTier::TypeOwned);
    }

    #[test]
    fn test_resolve_member_via_type_member_on_resolved_base_returns_type_heritage() {
        let mut st = inherited_greet();
        st.build_supertypes([(2, Some(0))]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Derived.greet", &[]);
        assert_eq!(out, vec![(1, HERITAGE)]);
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    #[test]
    fn test_resolve_member_via_type_no_supertypes_returns_unresolved() {
        let st = inherited_greet();
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Derived.greet", &[]);
        assert!(out.is_empty(), "closed heritage, no owner: got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }

    #[test]
    fn test_resolve_member_via_type_external_base_before_owner_returns_no_edge() {
        // `class Derived(ExternalMixin, Base)`: the mixin comes first in the
        // MRO and may define `greet`.
        let mut st = inherited_greet();
        st.build_supertypes([(2, None), (2, Some(0))]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Derived.greet", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }

    #[test]
    fn test_resolve_member_via_type_external_base_after_owner_returns_type_heritage() {
        let mut st = inherited_greet();
        st.build_supertypes([(2, Some(0)), (2, None)]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Derived.greet", &[]);
        assert_eq!(out, vec![(1, HERITAGE)]);
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    #[test]
    fn test_resolve_member_via_type_owners_on_two_branches_returns_no_edge() {
        // `class T(A, B)` with `greet` on both A and B: the winner depends on
        // the language's method resolution order.
        let mut st = st_owned(&[
            ("pkg/a.py", "A", NodeKind::Class, None),
            ("pkg/a.py", "greet", NodeKind::Method, Some("A")),
            ("pkg/b.py", "B", NodeKind::Class, None),
            ("pkg/b.py", "greet", NodeKind::Method, Some("B")),
            ("pkg/t.py", "T", NodeKind::Class, None),
        ]);
        st.build_supertypes([(4, Some(0)), (4, Some(2))]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "T.greet", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }

    #[test]
    fn test_resolve_member_via_type_override_chain_returns_most_derived_owner() {
        // T -> A -> B, both A and B define `greet`: A overrides B.
        let mut st = st_owned(&[
            ("pkg/a.py", "A", NodeKind::Class, None),
            ("pkg/a.py", "greet", NodeKind::Method, Some("A")),
            ("pkg/b.py", "B", NodeKind::Class, None),
            ("pkg/b.py", "greet", NodeKind::Method, Some("B")),
            ("pkg/t.py", "T", NodeKind::Class, None),
        ]);
        st.build_supertypes([(4, Some(0)), (0, Some(2))]);
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "T.greet", &[]);
        assert_eq!(out, vec![(1, HERITAGE)]);
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    /// Two `Repo` types in sibling packages. Ids: a.Repo 0, a.Repo.save 1,
    /// b.Repo 2, b.Repo.load 3.
    fn two_repos() -> SymbolTable {
        st_owned(&[
            ("app/a/models.py", "Repo", NodeKind::Class, None),
            ("app/a/models.py", "save", NodeKind::Method, Some("Repo")),
            ("app/b/models.py", "Repo", NodeKind::Class, None),
            ("app/b/models.py", "load", NodeKind::Method, Some("Repo")),
        ])
    }

    #[test]
    fn test_resolve_member_via_type_ambiguous_qualifier_one_owner_returns_type_candidates() {
        let st = two_repos();
        let (out, tier) = resolve_dumped(&st, "app/c.py", "Repo.save", &[]);
        assert_eq!(out, vec![(1, ResolutionTier::Global.base_confidence())]);
        assert_eq!(tier, DecisionTier::TypeCandidates);
    }

    #[test]
    fn test_resolve_member_via_type_ambiguous_qualifier_one_unknown_returns_ambiguous_global() {
        // b.Repo has an external base that may define `save`.
        let mut st = two_repos();
        st.build_supertypes([(2, None)]);
        let (out, tier) = resolve_dumped(&st, "app/c.py", "Repo.save", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::AmbiguousGlobal);
    }

    #[test]
    fn test_resolve_member_via_type_module_qualifier_returns_unresolved() {
        // `cfg` is a module, not a type; a member owned under that name in
        // another file must not resolve through the ladder.
        let st = st_owned(&[
            ("src/lib.rs", "cfg", NodeKind::Module, None),
            ("src/loader.rs", "load", NodeKind::Function, Some("cfg")),
        ]);
        let (out, tier) = resolve_dumped(&st, "src/app.rs", "cfg::load", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }

    #[test]
    fn test_resolve_member_via_type_library_import_returns_no_edge() {
        // `from psqlpy import Connection`: the project's own `Connection`
        // (whose base defines `execute`) is not this receiver's type.
        let mut st = st_owned(&[
            ("pkg/base.py", "BaseConn", NodeKind::Class, None),
            ("pkg/base.py", "execute", NodeKind::Method, Some("BaseConn")),
            ("pkg/models.py", "Connection", NodeKind::Class, None),
        ]);
        st.build_supertypes([(2, Some(0))]);
        let lib = [import("psqlpy", "Connection", None)];
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Connection.execute", &lib);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);

        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "Connection.execute", &[]);
        assert_eq!(
            out,
            vec![(1, HERITAGE)],
            "without the library import the project type resolves"
        );
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    #[test]
    fn test_resolve_member_via_type_aliased_project_import_uses_declared_name() {
        // `from .models import Repo as R` then `R.save()`.
        let mut st = st_owned(&[
            ("pkg/models.py", "Repo", NodeKind::Class, None),
            ("pkg/base.py", "BaseRepo", NodeKind::Class, None),
            ("pkg/base.py", "save", NodeKind::Method, Some("BaseRepo")),
        ]);
        st.build_supertypes([(0, Some(1))]);
        let aliased = [import(".models", "Repo", Some("R"))];
        let (out, tier) = resolve_dumped(&st, "pkg/app.py", "R.save", &aliased);
        assert_eq!(out, vec![(2, HERITAGE)]);
        assert_eq!(tier, DecisionTier::TypeHeritage);
    }

    #[test]
    fn test_resolve_member_via_type_external_path_prefix_returns_unresolved() {
        // `std::sync::Arc::new` names std's Arc, not a project `Arc`.
        let st = st_owned(&[
            ("src/arc.rs", "Arc", NodeKind::Struct, None),
            ("src/arc_impl.rs", "new", NodeKind::Function, Some("Arc")),
        ]);
        let (out, tier) = resolve_dumped(&st, "src/app.rs", "std::sync::Arc::new", &[]);
        assert!(out.is_empty(), "got {out:?}");
        assert_eq!(tier, DecisionTier::Unresolved);
    }
}
