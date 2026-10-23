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

use ecp_core::analyzer::import_binding::{
    extension_retry, fqn_extension, fqn_imported_name, fqn_language, fqn_type_path, import_binding,
    import_family, import_member_fallback, may_suppress, module_head, retry_name,
};
use ecp_core::analyzer::path_aliases::PathAliases;
use ecp_core::analyzer::reexport::{
    self, is_ecmascript, probe_rust_module, ExportAliasTable, Fact, FileImports, ImportRef,
    ModuleFacts, PyModule,
};
use ecp_core::analyzer::rust_paths::{is_rust_source, rust_module_path_base};
use ecp_core::analyzer::same_file_pick::type_receiver_is_closed;
use ecp_core::analyzer::type_ladder::{
    self, Ownership, TypeCandidates, TypeMember as LadderMember,
};
use ecp_core::analyzer::types::{owner_key, CallSite, LocalGraph, RawImport};
use ecp_core::graph::NodeKind;
use serde::Serialize;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::resolution::heuristics::ResolutionTier;
use crate::resolution::index::{
    crate_root_prefix, FileMeta, GlobalPick, Language, PickScope, ResolveTarget, SymbolTable,
};
use crate::rust::module_tree::RustWorkspaceModTree;
use rustc_hash::{FxHashMap, FxHashSet};

pub type NodeId = u32;

#[derive(Clone)]
enum IndexedModule {
    Missing,
    Unique(Arc<str>),
    Ambiguous,
    Namespace(Arc<[Arc<str>]>),
}

/// One pass of the import-member tier over the explicit or the wildcard
/// imports.
enum MemberHit<'i> {
    /// The member, the import that bound it, and the Python package whose
    /// re-export it went through ([`reexport::python_reexport`]).
    Bound(NodeId, &'i RawImport, Option<Arc<str>>),
    /// No unique member. `bound` says whether an import of the pass binds
    /// the callee at all.
    Unbound { bound: bool },
}

type ModuleCache = FxHashMap<
    std::mem::Discriminant<Language>,
    FxHashMap<PathBuf, FxHashMap<String, IndexedModule>>,
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

    fn has_module_name(&self, language: Language, name: &str) -> bool {
        self.0.symbol_table.has_module_name(language, name)
    }

    fn declares(&self, file: &str, name: &str) -> bool {
        self.0.symbol_table.lookup_in_file(file, name).is_some()
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
            IndexedModule::Unique(file) => PyModule::Unique(file),
            IndexedModule::Ambiguous => PyModule::Ambiguous,
            IndexedModule::Missing | IndexedModule::Namespace(_) => PyModule::Missing,
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
/// only resolution *successes* (and has the unused `Fallback(...)` arm),
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
        }
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

    /// Find a module beneath an indexed source root without guessing its member.
    /// Relative specifiers stay anchored to their caller; absolute module paths
    /// may have a source-root prefix, but multiple matching roots are ambiguous.
    /// Only files of `language` count: the caller's, or for the suppression
    /// check another language of its import family.
    fn import_module_file(
        &self,
        source_file: &Path,
        specifier: &str,
        language: Language,
    ) -> IndexedModule {
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
            .or_insert_with(|| self.discover_import_module(source_file, specifier, language))
            .clone()
    }

    fn for_each_import_candidate(
        &self,
        source_file: &Path,
        specifier: &str,
        language: Language,
        mut visit: impl FnMut(&str) -> bool,
    ) {
        if language == Language::Python {
            let mut verbatim = true;
            self.for_each_candidate(source_file, specifier, |base| {
                if verbatim {
                    verbatim = false;
                    return true;
                }
                crate::python::spec::module_candidates(base, &mut visit);
                false
            });
        } else {
            self.for_each_candidate(source_file, specifier, visit);
        }
    }

    fn first_indexed_module(
        &self,
        source_file: &Path,
        specifier: &str,
        language: Language,
    ) -> Option<Arc<str>> {
        let mut found = None;
        self.for_each_import_candidate(source_file, specifier, language, |candidate| {
            if Language::from_normalized_path(candidate) == language
                && self.symbol_table.has_indexed_file(candidate)
            {
                found = Some(Arc::from(candidate));
                false
            } else {
                true
            }
        });
        found
    }

    fn discover_import_module(
        &self,
        source_file: &Path,
        specifier: &str,
        language: Language,
    ) -> IndexedModule {
        if let Some(extension) = fqn_extension(language) {
            return self.discover_fqn_module(specifier, language, extension);
        }
        let exact = self.first_indexed_module(source_file, specifier, language);
        if exact.is_some() || specifier.starts_with('.') {
            return exact.map_or(IndexedModule::Missing, IndexedModule::Unique);
        }
        let mut found: Option<(&str, Arc<str>)> = None;
        let mut ambiguous = false;
        self.for_each_import_candidate(source_file, specifier, language, |candidate| {
            if Language::from_normalized_path(candidate) != language {
                return true;
            }
            let Some(stem) = Path::new(candidate).file_stem().and_then(|s| s.to_str()) else {
                return true;
            };
            for file in self.symbol_table.module_files_by_stem(stem) {
                let Some(root) = file
                    .strip_suffix(candidate)
                    .filter(|prefix| prefix.ends_with('/'))
                else {
                    continue;
                };
                // A directory already indexed as a module is a package,
                // not an implicit source root for an absolute import.
                if self
                    .first_indexed_module(source_file, root.trim_end_matches('/'), language)
                    .is_some()
                {
                    continue;
                }
                if let Some((previous_root, _)) = &found {
                    if *previous_root != root {
                        ambiguous = true;
                        return false;
                    }
                    // Candidate order already selected this root's package or implementation.
                    continue;
                }
                found = Some((root, Arc::from(file)));
            }
            true
        });
        if ambiguous {
            IndexedModule::Ambiguous
        } else {
            found.map_or(IndexedModule::Missing, |(_, file)| {
                IndexedModule::Unique(file)
            })
        }
    }

    fn discover_fqn_module(
        &self,
        specifier: &str,
        language: Language,
        extension: &str,
    ) -> IndexedModule {
        let normalized = specifier.trim_start_matches('\\').replace('\\', ".");
        let mut name = normalized.as_str();
        loop {
            let files: Vec<Arc<str>> = self
                .symbol_table
                .namespace_files(name)
                .iter()
                .filter(|file| Language::from_normalized_path(file) == language)
                .map(|file| Arc::from(file.as_str()))
                .collect();
            if !files.is_empty() {
                let member = normalized
                    .strip_prefix(name)
                    .unwrap_or("")
                    .trim_start_matches('.')
                    .split('.')
                    .next()
                    .unwrap_or("");
                return IndexedModule::Namespace(
                    files
                        .into_iter()
                        .filter(|file| {
                            member.is_empty()
                                || self.symbol_table.lookup_in_file(file, member).is_some()
                        })
                        .collect::<Vec<_>>()
                        .into(),
                );
            }
            let candidate = format!("{}.{}", name.replace('.', "/"), extension);
            let stem = name.rsplit('.').next().unwrap_or(name);
            // A placed file that declares another namespace is a namesake:
            // `use InvalidArgumentException;` is not `Lib\Testing`'s class.
            let namespace = name.rsplit_once('.').map_or("", |(outer, _)| outer);
            let files: Vec<Arc<str>> = self
                .symbol_table
                .module_files_by_stem(stem)
                .filter(|file| {
                    Language::from_normalized_path(file) == language
                        && (*file == candidate
                            || file
                                .strip_suffix(&candidate)
                                .is_some_and(|root| root.ends_with('/')))
                        && !self.symbol_table.declares_other_namespace(file, namespace)
                })
                .map(Arc::from)
                .collect();
            if !files.is_empty() {
                return if files.len() == 1 {
                    IndexedModule::Unique(files[0].clone())
                } else {
                    IndexedModule::Ambiguous
                };
            }
            let Some((parent, _)) = name.rsplit_once('.') else {
                break;
            };
            name = parent;
        }
        IndexedModule::Missing
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

    /// Resolve one call site to its `Calls` targets: the callable tiers,
    /// then, only when they find nothing, the constructor fallback
    /// ([`Self::resolve_instantiation`]). The fallback lives here, not in
    /// [`Self::resolve_symbol_with_heritage`], because that method also
    /// serves References / Decorates / Imports edges, which must never land
    /// on a type through a construction rule. `caller_owner` is the type the
    /// calling member belongs to, `caller` the calling declaration
    /// (`SymbolTable::prefer_owned`).
    ///
    /// A member call reaches a method or a type's function, never a type or
    /// its constructor by the type's name: an untyped `o.m()`
    /// ([`CallSite::UntypedMember`]) never takes the constructor fallback,
    /// and every tier's target passes `SymbolTable::member_call_admits` (no
    /// free function, no namespace member, no constructor, no member of the
    /// caller's own type by the name alone). Neither does a bare `m()` that
    /// may be a member call whose receiver the parser dropped take the
    /// fallback ([`Self::may_be_receiverless_member_call`]).
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_call(
        &self,
        source_file: &Path,
        site: CallSite<'_>,
        raw_imports: &[RawImport],
        caller_heritage: &[String],
        caller_owner: Option<&str>,
        caller: Option<NodeId>,
    ) -> Vec<(NodeId, f32)> {
        // A chain the caller did not type ([`Self::resolve_chained_member`])
        // resolves as the entry the parser wrote before chains.
        let site = site.fallback();
        let language = Language::from_normalized_path(&normalize_source_path(source_file));
        if let Some((node_id, import)) =
            self.python_submodule_member(source_file, language, site, raw_imports)
        {
            return self.import_scoped(source_file, site.name(), import, node_id);
        }
        let declared_python_attribute = matches!(site, CallSite::FromReceiver { receiver, .. }
        if language == Language::Python
            && receiver.contains('/')
            && reexport::python_from_import_declared_attribute(
                &IndexFacts(self),
                &normalize_source_path(source_file),
                receiver,
                raw_imports.iter().map(ImportRef::from),
            ));
        let type_member = self.type_receiver(source_file, language, site, raw_imports);
        // A Swift receiver that names no type is a value the parser cannot
        // see (a global, an inherited property): an untyped member call.
        let site = match (site, &type_member) {
            (CallSite::FromReceiver { member, .. }, None) if language == Language::Swift => {
                CallSite::UntypedMember(member)
            }
            (CallSite::FromReceiver { member, .. }, None) if declared_python_attribute => {
                CallSite::UntypedMember(member)
            }
            _ => site,
        };
        let bind_import = !matches!(site, CallSite::UntypedMember(_));
        let free_only = site.reaches_free_only(language);
        // An untyped receiver is no `this`: no tier reaches the member
        // through the caller's bases.
        let heritage: &[String] = if bind_import { caller_heritage } else { &[] };
        let resolve = |name: &str, target, bind: bool| {
            self.resolve_symbol_with_import_binding(
                source_file,
                name,
                language,
                (raw_imports, bind && bind_import),
                target,
                heritage,
                Some(CallScope {
                    owner: caller_owner,
                    untyped_receiver: !bind_import,
                    free_only,
                    caller,
                }),
                &mut None,
            )
        };
        let target = call_target(source_file, site);
        let mut targets = resolve(type_member.as_deref().unwrap_or(site.name()), target, true);
        if targets.is_empty() && type_member.is_some() && language == Language::Kotlin {
            // `fun Reg.ext()` is a top-level function no member lookup on
            // `Reg` finds.
            targets =
                self.extension_fallback(source_file, site.name(), raw_imports, target, heritage);
        }
        if !bind_import {
            // A Python module is a value: `api.helper()` on a receiver the
            // parser could not type may call another module's function. No
            // value reaches the caller's own module that way.
            let source_file_str = normalize_source_path(source_file);
            let module_value = |id: NodeId| {
                language == Language::Python
                    && self.symbol_table.is_top_level(id)
                    && self.symbol_table.file_of(id) != Some(source_file_str.as_ref())
            };
            targets.retain(|&(id, _)| {
                self.symbol_table.member_call_admits(id, caller_owner) || module_value(id)
            });
        }
        if targets.is_empty() && !self.may_be_receiverless_member_call(source_file, site) {
            targets.extend(self.resolve_instantiation(language, site, raw_imports, resolve));
        }
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
    pub fn resolve_chained_member(
        &self,
        source_file: &Path,
        (callee, inner_confidence): (NodeId, f32),
        member: &str,
    ) -> Option<(NodeId, f32)> {
        let st = self.symbol_table;
        let ty = type_ladder::returned_type(st, callee)?;
        let callee_file = st.file_of(callee)?;
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
                        [(id, _)] => TypeCandidates::Unique(*id, st.name_in_file(*id)?),
                        _ => return None,
                    }
                }
                Language::CSharp
                    if imports
                        .iter()
                        .any(|import| import.alias.as_deref() == Some(ty)) =>
                {
                    let path = imports
                        .iter()
                        .find(|import| import.alias.as_deref() == Some(ty))?
                        .imported_name
                        .as_str();
                    let name = owner_key(path)?;
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
                _ if st.declares_type_beside(callee_file, ty) => {
                    self.type_candidates(source, callee_file, ty, &[])
                }
                _ => return None,
            }
        };
        let target = call_target(source_file, CallSite::UntypedMember(member));
        let (id, tier) =
            match type_ladder::member_via_type(st, candidates, callee_file, member, target, true) {
                LadderMember::Owned(id) => (id, ResolutionTier::QualifierScoped),
                LadderMember::Inherited(id) => (id, ResolutionTier::HeritageScoped),
                LadderMember::Candidate(id) => (id, ResolutionTier::Global),
                LadderMember::NearestCopy(id) => (id, ResolutionTier::NearestCopy),
                LadderMember::Ambiguous | LadderMember::NoEdge => return None,
            };
        Some((id, tier.base_confidence().min(inner_confidence)))
    }

    /// Whether a bare `m()` that the constructor fallback would read as a
    /// construction of a type `m` may instead be a member call whose
    /// receiver the parser dropped. The C++, Swift and Ruby parsers now
    /// write an untyped receiver as [`CallSite::UntypedMember`], which never
    /// constructs; a parser that still drops one (Kotlin and Dart write a
    /// receiver they bind to a field or local as the bare member) reaches
    /// here. When an admitted `Method` is named `m`, the name is a member's,
    /// and `m(x)` never binds a type `m` of another file (`template
    /// <typename T> class number_integer` in a test file for nlohmann's
    /// `sax->number_integer(v)`). A call written with a qualifier, a `new`,
    /// or a Ruby `.new` is no such site.
    fn may_be_receiverless_member_call(&self, source_file: &Path, site: CallSite<'_>) -> bool {
        let CallSite::Plain(callee) = site else {
            return false;
        };
        !callee.contains(&['.', ':'][..])
            && self.symbol_table.has_constructible_type(callee)
            && self.symbol_table.names_admitted_method(
                callee,
                FileMeta::from_normalized_path(&normalize_source_path(source_file)),
            )
    }

    /// The callee of a C#, Swift, Kotlin or Dart member call on an
    /// identifier receiver that is no local, parameter, field or property in
    /// scope ([`CallSite::FromReceiver`]). A receiver that names a project
    /// type, a `using` alias of one, or a library type resolves as
    /// `Receiver.member` through the qualifier tiers and the receiver-typing
    /// ladder, and never by the bare member: `StringAssert.AreEqual()` must
    /// not land on `XUnitAssert.AreEqual`, nor Swift `Util.make()` on the
    /// caller's own `make`. A receiver that names no type is a member the
    /// parser cannot see (an inherited property, a global): `None`.
    ///
    /// A Python `FromReceiver` that is no submodule
    /// ([`Self::python_submodule_member`] runs first) names a type only when
    /// its module file declares one ([`reexport::python_imported_type`]):
    /// `from k import K; K.st()` with several project `st` would otherwise
    /// bind none.
    fn type_receiver(
        &self,
        source_file: &Path,
        language: Language,
        site: CallSite<'_>,
        raw_imports: &[RawImport],
    ) -> Option<String> {
        let CallSite::FromReceiver { receiver, member } = site else {
            return None;
        };
        let source_file_str = normalize_source_path(source_file);
        let typed = match language {
            Language::Python => reexport::python_imported_type(
                &IndexFacts(self),
                &source_file_str,
                receiver,
                raw_imports.iter().map(ImportRef::from),
            ),
            Language::CSharp | Language::Swift | Language::Kotlin | Language::Dart => !matches!(
                self.type_candidates(source_file, &source_file_str, receiver, raw_imports),
                TypeCandidates::None
            ),
            _ => false,
        };
        typed.then(|| format!("{receiver}.{member}"))
    }

    /// [`Self::resolve_call`] for a name a lexical scope of the file binds,
    /// though not one enclosing the call: only an import may resolve it,
    /// never the inaccessible local, and the constructor fallback follows
    /// the same rule.
    pub fn resolve_imported_call(
        &self,
        source_file: &Path,
        site: CallSite<'_>,
        raw_imports: &[RawImport],
    ) -> Vec<(NodeId, f32)> {
        let site = site.fallback();
        if matches!(site, CallSite::UntypedMember(_)) {
            return Vec::new();
        }
        let language = Language::from_normalized_path(&normalize_source_path(source_file));
        if let Some((node_id, import)) =
            self.python_submodule_member(source_file, language, site, raw_imports)
        {
            return self.import_scoped(source_file, site.name(), import, node_id);
        }
        if matches!(site, CallSite::FromReceiver { receiver, .. }
        if language == Language::Python
            && receiver.contains('/')
            && reexport::python_from_import_declared_attribute(
                &IndexFacts(self),
                &normalize_source_path(source_file),
                receiver,
                raw_imports.iter().map(ImportRef::from),
            ))
        {
            return Vec::new();
        }
        let resolve = |name: &str, target, bind: bool| {
            self.resolve_import_tiers(
                source_file,
                language,
                name,
                (raw_imports, bind),
                target,
                &mut None,
            )
        };
        let mut targets = resolve(site.name(), call_target(source_file, site), true);
        if targets.is_empty() {
            targets.extend(self.resolve_instantiation(language, site, raw_imports, resolve));
        }
        targets
    }

    /// Constructor fallback for a call site the callable tiers left empty:
    /// resolve the constructed type through `resolve` with
    /// [`ResolveTarget::Type`], keep a Class / Struct, then land on its one
    /// constructor, or on the type itself when it declares none or several.
    ///
    /// A qualified type path that does not resolve (`new shop.Widget()`
    /// through a namespace import, PHP `new \App\Item()`) retries its last
    /// segment only where [`CallSite::constructed_type`] allows it; a plain
    /// `pkg.A()` stays unresolved, like every qualified callee. The third
    /// argument of `resolve` says whether the import-member tier may bind.
    fn resolve_instantiation(
        &self,
        language: Language,
        site: CallSite<'_>,
        raw_imports: &[RawImport],
        resolve: impl Fn(&str, ResolveTarget, bool) -> Vec<(NodeId, f32)>,
    ) -> Option<(NodeId, f32)> {
        let (type_path, last_segment_fallback) = site.constructed_type(language)?;
        let type_name = split_qualifier(type_path).map_or(type_path, |(_, member)| member);
        if !self.names_constructible_type(type_name, raw_imports) {
            return None;
        }
        let mut types = resolve(type_path, ResolveTarget::Type, true);
        if types.is_empty() && last_segment_fallback && type_name.len() < type_path.len() {
            // A qualified type path (`new \App\User()`) never names its type
            // through a `use` of its last segment.
            types = resolve(type_name, ResolveTarget::Type, !fqn_language(language));
        }
        let &[(type_id, confidence)] = types.as_slice() else {
            return None;
        };
        if !self.symbol_table.node_kind(type_id).is_constructible() {
            return None;
        }
        let target = self
            .symbol_table
            .sole_constructor(type_id)
            .unwrap_or(type_id);
        Some((target, confidence))
    }

    /// The cheap gate of [`Self::resolve_instantiation`]: some Class /
    /// Struct is declared as `name`, or an import aliases a declared one to
    /// it (`import { Widget as Renamed }`), which only the import tier maps
    /// back.
    fn names_constructible_type(&self, name: &str, raw_imports: &[RawImport]) -> bool {
        self.symbol_table.has_constructible_type(name)
            || raw_imports.iter().any(|import| {
                import.alias.as_deref() == Some(name)
                    && self
                        .symbol_table
                        .has_constructible_type(&import.imported_name)
            })
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
        // may bind (Java `import static app.Consts.TABLE`); `resolve_symbol`
        // keeps `obj.field` reads off the import tiers on its own.
        if raw_imports.is_empty() {
            return Vec::new();
        }
        if !(bind_import && import_member_fallback(language)) {
            let (targets, via) =
                self.resolve_named_import(source_file, symbol_name, raw_imports, target);
            *barrel = via;
            return targets;
        }
        let hit = match self.import_member_hit(
            source_file,
            language,
            symbol_name,
            raw_imports,
            target,
            false,
        ) {
            MemberHit::Unbound { bound: false } => self.import_member_hit(
                source_file,
                language,
                symbol_name,
                raw_imports,
                target,
                true,
            ),
            explicit => explicit,
        };
        match hit {
            MemberHit::Bound(node_id, import, via) => {
                *barrel = via;
                self.import_scoped(source_file, symbol_name, import, node_id)
            }
            MemberHit::Unbound { .. } => Vec::new(),
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
                ),
                hit.barrel,
            ),
            None => (Vec::new(), None),
        }
    }

    /// One pass of the import-member tier: the member the explicit (or, with
    /// `wildcard`, the wildcard) imports binding `symbol_name` hold in their
    /// in-repo modules. Python keeps the first hit in source order, as Python
    /// rebinds a name per import. In a fully qualified language two imports
    /// that hold different members are no hit: the language rejects the
    /// call or picks by signature, so the general tiers decide.
    fn import_member_hit<'i>(
        &self,
        source_file: &Path,
        language: Language,
        symbol_name: &str,
        raw_imports: &'i [RawImport],
        target: ResolveTarget,
        wildcard: bool,
    ) -> MemberHit<'i> {
        let mut found: Option<(NodeId, &'i RawImport)> = None;
        let mut bound = false;
        for import in raw_imports {
            let Some(binding) = import_binding(import, symbol_name, language)
                .filter(|binding| binding.wildcard == wildcard)
            else {
                continue;
            };
            bound = true;
            let in_file = |file: &str| {
                self.symbol_table
                    .lookup_member_in_file(file, binding.name, target, binding.owner)
            };
            let hit = match self.import_module_file(source_file, &import.source, language) {
                IndexedModule::Unique(module) => match in_file(&module) {
                    Some(node_id) => Some((node_id, None)),
                    None => reexport::python_import_reexport(
                        &IndexFacts(self),
                        language,
                        &module,
                        binding.name,
                        target,
                    )
                    .map(|node_id| (node_id, Some(module))),
                },
                IndexedModule::Namespace(files) => {
                    let mut hits = files.iter().filter_map(|file| in_file(file));
                    hits.next()
                        .filter(|_| hits.next().is_none())
                        .map(|node_id| (node_id, None))
                }
                IndexedModule::Missing | IndexedModule::Ambiguous => None,
            };
            let Some((node_id, barrel)) = hit else {
                continue;
            };
            if !fqn_language(language) {
                return MemberHit::Bound(node_id, import, barrel);
            }
            match found {
                Some((previous, _)) if previous != node_id => {
                    return MemberHit::Unbound { bound };
                }
                Some(_) => {}
                None => found = Some((node_id, import)),
            }
        }
        found.map_or(MemberHit::Unbound { bound }, |(node_id, import)| {
            MemberHit::Bound(node_id, import, None)
        })
    }

    /// `from pkg.sub import mod; mod.compute()` through
    /// [`reexport::python_submodule_member`]. `None` on any miss: the call
    /// then resolves as the bare member, as before.
    fn python_submodule_member<'i>(
        &self,
        source_file: &Path,
        language: Language,
        site: CallSite<'_>,
        raw_imports: &'i [RawImport],
    ) -> Option<(NodeId, &'i RawImport)> {
        let CallSite::FromReceiver { receiver, member } = site else {
            return None;
        };
        if language != Language::Python {
            return None;
        }
        let (node_id, position) = reexport::python_submodule_member(
            &IndexFacts(self),
            &normalize_source_path(source_file),
            (receiver, member),
            raw_imports.iter().map(ImportRef::from),
            call_target(source_file, site),
        )?;
        Some((node_id, &raw_imports[position]))
    }

    /// Whether `import` proves its callee external: [`may_suppress`] allows
    /// it, and no language of its [`import_family`] indexes a module name
    /// equal to its head or holds its module. Missing discovery alone is not
    /// proof: a wrong suppression drops a genuine caller.
    fn import_is_external(
        &self,
        source_file: &Path,
        import: &RawImport,
        language: Language,
    ) -> bool {
        let head = module_head(&import.source);
        may_suppress(import, language)
            && import_family(language)
                .all(|family| !self.symbol_table.has_module_name(family, head))
            && import_family(language).all(|family| {
                matches!(
                    self.import_module_file(source_file, &import.source, family),
                    IndexedModule::Missing
                )
            })
    }

    /// Record and return an ImportScoped edge to `node_id` through `import`.
    fn import_scoped(
        &self,
        source_file: &Path,
        symbol_name: &str,
        import: &RawImport,
        node_id: NodeId,
    ) -> Vec<(NodeId, f32)> {
        let confidence = ResolutionTier::ImportScoped.base_confidence();
        self.record(
            &normalize_source_path(source_file),
            symbol_name,
            Some(import.source.as_str()),
            DecisionTier::ImportScoped,
            Some(node_id),
            0,
            Some(confidence),
        );
        vec![(node_id, confidence)]
    }

    /// The bare-member retry of a Kotlin call through a class binding
    /// ([`extension_retry`]). Only a top-level function can be an extension
    /// on that type: a typed receiver of a library type (`gson.fromJson()`)
    /// or `super.onResume()` must not land on a same-named member.
    /// Extension receivers are not indexed, so the top-level retry remains
    /// a by-name heuristic and cannot verify the declared receiver type.
    pub(crate) fn extension_fallback(
        &self,
        source_file: &Path,
        member: &str,
        raw_imports: &[RawImport],
        target: ResolveTarget,
        caller_heritage: &[String],
    ) -> Vec<(NodeId, f32)> {
        let mut hits = self.resolve_symbol_with_heritage(
            source_file,
            member,
            raw_imports,
            target,
            caller_heritage,
        );
        hits.retain(|&(id, _)| self.symbol_table.is_top_level(id));
        hits
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

    #[allow(clippy::too_many_arguments)]
    fn resolve_symbol_with_import_binding(
        &self,
        source_file: &Path,
        symbol_name: &str,
        language: Language,
        (raw_imports, bind_import): (&[RawImport], bool),
        target: ResolveTarget,
        caller_heritage: &[String],
        call: Option<CallScope<'_>>,
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
        let same_file_pick =
            self.symbol_table
                .lookup_call_in_file(&source_file_str, symbol_name, target, free_only);
        if let Some(node_id) = same_file_pick
            .and_then(|pick| match call {
                Some(scope) if free_only => ecp_core::analyzer::same_file_pick::prefer_free(
                    self.symbol_table,
                    &source_file_str,
                    symbol_name,
                    scope.owner,
                    target,
                ),
                Some(scope) => self.symbol_table.prefer_owned(
                    &source_file_str,
                    pick,
                    symbol_name,
                    scope.owner,
                    if scope.untyped_receiver {
                        &[]
                    } else {
                        caller_heritage
                    },
                    target,
                    PickScope {
                        caller: scope.caller,
                        typed_receiver: false,
                        untyped_receiver: scope.untyped_receiver,
                        qualifier_is_type: false,
                    },
                ),
                None => Some(pick),
            })
            .filter(|&id| !free_only || self.symbol_table.node_kind(id) != NodeKind::Method)
        {
            results.push((node_id, ResolutionTier::SameFile.base_confidence()));
            self.record(
                &source_file_str,
                symbol_name,
                None,
                DecisionTier::SameFile,
                Some(node_id),
                0,
                Some(ResolutionTier::SameFile.base_confidence()),
            );
            return results; // Highest precedence, return early
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
            match self.import_member_hit(
                source_file,
                language,
                symbol_name,
                raw_imports,
                target,
                false,
            ) {
                MemberHit::Bound(node_id, import, via) => {
                    *barrel = via;
                    return self.import_scoped(source_file, symbol_name, import, node_id);
                }
                MemberHit::Unbound { bound } => wildcard_open = !bound && fqn_language(language),
            }
            let mut retry = None;
            let mut extension = None;
            let mut bound = false;
            let mut external = true;
            for import in raw_imports {
                let Some(binding) = import_binding(import, symbol_name, language) else {
                    continue;
                };
                bound = true;
                retry = retry_name(import, binding.name, symbol_name, language);
                extension = extension.or(extension_retry(&binding, language));
                external = external && self.import_is_external(source_file, import, language);
            }
            if bound {
                // `super.x()` names the caller's base, which no extension
                // function call does.
                let heritage_call = split_qualifier(symbol_name)
                    .is_some_and(|(qualifier, _)| caller_heritage.iter().any(|b| b == qualifier));
                if let (true, Some(member), false) = (external, extension, heritage_call) {
                    return self.extension_fallback(
                        source_file,
                        member,
                        raw_imports,
                        target,
                        caller_heritage,
                    );
                }
                if external {
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
                if let Some(retry) = retry {
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
            }
        } else if has_imports {
            let (imported, via) =
                self.resolve_named_import(source_file, symbol_name, raw_imports, target);
            if !imported.is_empty() {
                *barrel = via;
                return imported;
            }
        }

        // Tier 2.5: Qualifier-scoped lookup. Callees that carry a qualifier
        // (`A::new`, `std::vec::Vec::new`, `Cls.method`) cannot match Tier 1/2
        // which are keyed by short names; without this tier they fall through
        // to Tier 3, where the kind+unique filter near-always rejects the
        // ultra-common member name (`new`, `default`, `from`, ...). Splitting
        // and scoping to the qualifier's defining file is the proper fix.
        //
        // No fall-through to Tier 3 on a short-name retry: a qualified callee
        // should resolve via its qualifier or not at all — matching the
        // "refuse to guess" principle that drives the Layer-1 barriers.
        //
        // Concretely, allowing a bare-name fallback would re-introduce a class
        // of pre-existing false edges that this tier was meant to remove:
        // `std::fs::read` stripping to `read` and resolving to a same-named
        // local function, `serde_json::json!` macro calls resolving to a
        // local `json()` helper, etc. Dump verification of B.1 vs B.1+fallback
        // showed a ~52% false-positive rate on the fallback path, so we keep
        // the strict policy. Module-qualified free functions like
        // `registry::sanitize_branch` whose member is uniquely defined will
        // be recovered by Phase B.3 (config-aware import resolution) once the
        // workspace crate index can distinguish `ecp_core::...` (internal,
        // safe to fall back) from `std::...` (external, refuse).
        if let Some((qualifier, member)) = split_qualifier(symbol_name) {
            if qualifier == CallSite::SUPER_RECEIVER
                && FileMeta::from_path(&source_file_str).language == Language::Python
            {
                return self.resolve_super_member(
                    source_file,
                    &source_file_str,
                    symbol_name,
                    member,
                    target,
                    raw_imports,
                    caller_heritage,
                );
            }
            // `this.m()` reaches here typed as the caller's own type (`C.m`),
            // whose bases are the caller's heritage. A type the index does
            // not declare (Rust `impl W for Pin<P>`) is still the caller's,
            // so its file is the caller's own.
            let caller_type = call
                .and_then(|scope| scope.owner)
                .and_then(owner_key)
                .is_some_and(|owner| owner_key(qualifier) == Some(owner));
            let hit = self
                .resolve_qualifier_file(
                    source_file,
                    qualifier,
                    member,
                    target,
                    raw_imports,
                    Some(symbol_name),
                )
                .or_else(|| caller_type.then(|| source_file_str.to_string()))
                .and_then(|qf| {
                    let heritage: &[String] = if caller_type { caller_heritage } else { &[] };
                    // A module or namespace qualifier (`mod.f()`) may reach a
                    // free function; a type reaches only its own lineage.
                    let qualifier_is_type = type_receiver_is_closed(language)
                        && (caller_type
                            || owner_key(qualifier).is_some_and(|key| {
                                self.symbol_table
                                    .lookup_in_file_with_kind(&qf, key, ResolveTarget::Type)
                                    .is_some()
                            }));
                    self.symbol_table
                        .lookup_in_file_with_kind(&qf, member, target)
                        .and_then(|pick| {
                            self.symbol_table.prefer_owned(
                                &qf,
                                pick,
                                member,
                                Some(qualifier),
                                heritage,
                                target,
                                PickScope {
                                    caller: call.and_then(|scope| scope.caller),
                                    typed_receiver: true,
                                    qualifier_is_type,
                                    ..PickScope::default()
                                },
                            )
                        })
                });
            if let Some(node_id) = hit {
                let conf = ResolutionTier::QualifierScoped.base_confidence();
                results.push((node_id, conf));
                self.record(
                    &source_file_str,
                    symbol_name,
                    None,
                    DecisionTier::QualifierScoped,
                    Some(node_id),
                    0,
                    Some(conf),
                );
                return results;
            }

            // Tier 3.5: Rust workspace module-tree FQN resolution.
            //
            // Fires when Tier 2.5 (qualifier-scoped) fails AND a Rust module
            // tree is available. Handles `crate::a::b::fn` and
            // `<crate_name>::a::b::fn` paths by walking the filesystem-backed
            // mod tree instead of relying on the qualifier-as-Type heuristic.
            //
            // Only fires for callee strings with `::` — dot-separated callees
            // are method calls (handled by heritage / Tier 2.5 via receiver
            // types) not module-path FQNs.
            if symbol_name.contains("::") {
                if let Some(node_id) =
                    self.try_module_tree_resolve(&source_file_str, symbol_name, member, target)
                {
                    const MT_CONF: f32 = 1.0;
                    results.push((node_id, MT_CONF));
                    self.record(
                        &source_file_str,
                        symbol_name,
                        None,
                        DecisionTier::ModuleTree,
                        Some(node_id),
                        0,
                        Some(MT_CONF),
                    );
                    return results;
                }
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
            // A Kotlin receiver named by a class import reaches here as
            // `Type.member` only since the import tier; before it the call was
            // the bare member, which finds an extension function declared on
            // the type in another file. A heritage qualifier (`super.x()`)
            // was always qualified, so it keeps this verdict.
            if matches!(tier, DecisionTier::Unresolved)
                && language == Language::Kotlin
                && !caller_heritage.iter().any(|base| base == qualifier)
                && raw_imports.iter().any(|import| {
                    import_binding(import, symbol_name, language)
                        .and_then(|binding| extension_retry(&binding, language))
                        .is_some()
                })
            {
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
        if !caller_heritage.is_empty()
            && !free_only
            && !call.is_some_and(|scope| scope.untyped_receiver)
        {
            for base in caller_heritage {
                if let Some(qf) = self.resolve_qualifier_file(
                    source_file,
                    base,
                    symbol_name,
                    target,
                    raw_imports,
                    None,
                ) {
                    if let Some(node_id) = self
                        .symbol_table
                        .lookup_in_file_with_kind(&qf, symbol_name, target)
                        .and_then(|pick| {
                            self.symbol_table.prefer_owned(
                                &qf,
                                pick,
                                symbol_name,
                                Some(base.as_str()),
                                &[],
                                target,
                                PickScope {
                                    typed_receiver: true,
                                    ..PickScope::default()
                                },
                            )
                        })
                    {
                        let conf = ResolutionTier::HeritageScoped.base_confidence();
                        results.push((node_id, conf));
                        self.record(
                            &source_file_str,
                            symbol_name,
                            Some(base.as_str()),
                            DecisionTier::HeritageScoped,
                            Some(node_id),
                            0,
                            Some(conf),
                        );
                        return results;
                    }
                }
            }
        }

        if wildcard_open {
            if let MemberHit::Bound(node_id, import, via) = self.import_member_hit(
                source_file,
                language,
                symbol_name,
                raw_imports,
                target,
                true,
            ) {
                *barrel = via;
                return self.import_scoped(source_file, symbol_name, import, node_id);
            }
        }

        // Tier 3: Global fallback — emit only when the kind-filtered candidate
        // set is unique. Refusing to guess on ambiguity is the dominant defence
        // against bare-name fan-out (`new`, `format`, `default`, `main`, ...).
        // `alt_count` in the dump still surfaces the raw same-name count so the
        // verification harness can distinguish suppressed-ambiguous from
        // truly-unresolved.
        let specifier = raw_imports
            .iter()
            .find(|i| match &i.alias {
                Some(a) => a == symbol_name,
                None => i.imported_name == symbol_name,
            })
            .map(|i| i.source.as_str());
        let raw_count = self.symbol_table.global_match_count(symbol_name);
        let caller_meta = FileMeta::from_path(&source_file_str);

        let hidden = |node_id: NodeId| {
            self.symbol_table.file_of(node_id).is_some_and(|module| {
                reexport::import_hides(
                    &IndexFacts(self),
                    &source_file_str,
                    symbol_name,
                    raw_imports.iter().map(ImportRef::from),
                    module,
                )
            })
        };
        let (tier, target_id, confidence, alt_count) =
            match self
                .symbol_table
                .lookup_global(symbol_name, target, caller_meta, free_only)
            {
                GlobalPick::Unique(node_id) if hidden(node_id) => {
                    (DecisionTier::Unresolved, None, None, raw_count)
                }
                GlobalPick::Unique(node_id) => {
                    let conf = ResolutionTier::Global.base_confidence();
                    results.push((node_id, conf));
                    (
                        DecisionTier::Global,
                        Some(node_id),
                        Some(conf),
                        raw_count.saturating_sub(1),
                    )
                }
                GlobalPick::Ambiguous => match call
                    .and_then(|_| {
                        self.symbol_table.nearest_global_copy(
                            &source_file_str,
                            caller_meta,
                            symbol_name,
                            target,
                            free_only,
                        )
                    })
                    .filter(|&node_id| !hidden(node_id))
                {
                    Some(node_id) => {
                        let conf = ResolutionTier::NearestCopy.base_confidence();
                        results.push((node_id, conf));
                        (
                            DecisionTier::NearestCopy,
                            Some(node_id),
                            Some(conf),
                            raw_count.saturating_sub(1),
                        )
                    }
                    None => (DecisionTier::AmbiguousGlobal, None, None, raw_count),
                },
                GlobalPick::NoMatch => (DecisionTier::Unresolved, None, None, raw_count),
            };
        self.record(
            &source_file_str,
            symbol_name,
            specifier,
            tier,
            target_id,
            alt_count,
            confidence,
        );

        results
    }
}

/// Split a qualified callee into `(qualifier, member)` where `qualifier` is
/// the **immediate** identifier preceding the rightmost separator. Returns
/// `None` if the name has no `::` / `.` separator or either side is empty.
///
/// For multi-segment paths only the last segment is taken as the qualifier
/// (the only piece the resolver can map back to a registered Type name —
/// `std::vec::Vec` is registered as just `Vec` keyed by its defining file).
///
/// Examples:
/// * `A::new` → `Some(("A", "new"))`
/// * `std::vec::Vec::new` → `Some(("Vec", "new"))`
/// * `obj.method` → `Some(("obj", "method"))`
/// * `foo` → `None`
///
/// Is the prefix preceding `qualifier` inside `full_callee` an "internal"
/// path (empty, or `crate` / `self` / `super` chain)? Tier 4 only fires on
/// calls where the qualifier names an internal module rather than the
/// trailing segment of an extern crate / std module path. `std::fs::read`
/// → preceding = `std` → returns false. `auto_ensure::ensure_fresh` →
/// preceding = `` → returns true. `crate::auto_ensure::ensure_fresh` →
/// preceding = `crate` → returns true.
fn qualifier_prefix_is_internal(full_callee: &str, qualifier: &str) -> bool {
    let Some(member_split) = full_callee.rfind("::").or_else(|| full_callee.rfind('.')) else {
        return false;
    };
    let before_member = &full_callee[..member_split];
    let preceding = before_member
        .rsplit_once("::")
        .or_else(|| before_member.rsplit_once('.'))
        .map(|(p, q)| if q == qualifier { p } else { "" })
        .unwrap_or("");
    preceding.is_empty()
        || preceding
            .split("::")
            .all(|s| matches!(s, "crate" | "self" | "super"))
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

/// For a Rust path call `a::b::Q::m`, does the module path `a::b` that the
/// call names for `Q` agree with the import of `Q` from `import_source`?
/// `de::Error::custom` with `use crate::error::Error` does not: `de::Error`
/// is another item than the imported `Error`. A head segment that a `use`
/// brings in (`use crate::error as err;`) expands to its full path first.
/// A `crate::` path must equal the import source; a relative one, with its
/// leading `self` / `super` dropped, must be a suffix of it. A call with no
/// module path before `Q`, or one not written with `::`, always agrees.
fn rust_path_prefix_agrees_with_import(
    full_callee: &str,
    qualifier: &str,
    import_source: &str,
    imports: &[RawImport],
) -> bool {
    let Some(prefix) = rust_type_path_prefix(full_callee, qualifier) else {
        return true;
    };
    let named = rust_expand_path_head(prefix, imports);
    // `crate::a::Q` names one module from the crate root: the import must
    // come from exactly that module. A relative path matches by suffix.
    if named.first() == Some(&"crate") {
        return import_source.split("::").eq(named);
    }
    let is_anchor = |s: &&str| matches!(*s, "" | "self" | "super");
    let named: Vec<&str> = named.into_iter().skip_while(is_anchor).collect();
    let source: Vec<&str> = import_source
        .split("::")
        .skip_while(|s| *s == "crate" || is_anchor(s))
        .collect();
    source.ends_with(&named)
}

/// The module path `a::b` that the Rust path call `a::b::Q::m` names before
/// its qualifier `Q`; `None` when there is none.
fn rust_type_path_prefix<'c>(full_callee: &'c str, qualifier: &str) -> Option<&'c str> {
    let (before_member, _) = full_callee.rsplit_once("::")?;
    let (prefix, q) = before_member.rsplit_once("::")?;
    (q == qualifier).then_some(prefix)
}

/// The segments of the Rust module path `prefix`, with a head that a `use`
/// brings in (`use crate::error as err;`) expanded to its full path.
fn rust_expand_path_head<'s>(prefix: &'s str, imports: &'s [RawImport]) -> Vec<&'s str> {
    let mut named: Vec<&str> = prefix.split("::").collect();
    if let Some(module) = named.first().and_then(|&head| {
        imports
            .iter()
            .find(|i| i.alias.as_deref().unwrap_or(&i.imported_name) == head)
    }) {
        // `use crate::error as err;` records the whole path as the imported
        // name; `use crate::{error as err};` records source `crate`, name
        // `error`.
        let name = module.imported_name.as_str();
        let (path, tail) = if name.contains("::") {
            (name, None)
        } else {
            (module.source.as_str(), Some(name))
        };
        named.splice(..1, path.split("::").chain(tail));
    }
    named
}

fn split_qualifier(name: &str) -> Option<(&str, &str)> {
    // Pick the rightmost separator. PHP's `\` is included alongside `::`
    // and `.` so `\App\helper` resolves with `App` as qualifier. `::` is
    // 2-char (preferred on tie since the position points at the first `:`,
    // not the `.`/`\` that might appear at the same column).
    let candidates = [
        (name.rfind("::"), 2usize),
        (name.rfind('.'), 1),
        (name.rfind('\\'), 1),
    ];
    let (split_idx, sep_len) = candidates
        .iter()
        .filter_map(|&(idx, len)| idx.map(|i| (i, len)))
        .max_by_key(|&(i, _)| i)?;
    let (before, after) = name.split_at(split_idx);
    let member = &after[sep_len..];
    if before.is_empty() || member.is_empty() {
        return None;
    }
    let qualifier = before
        .rsplit_once("::")
        .or_else(|| before.rsplit_once('.'))
        .or_else(|| before.rsplit_once('\\'))
        .map(|(_, q)| q)
        .unwrap_or(before);
    if qualifier.is_empty() {
        return None;
    }
    Some((qualifier, member))
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

/// What a call site adds to a name lookup ([`Resolver::resolve_call`]).
/// Its presence also admits [`ResolutionTier::NearestCopy`], whose
/// confidence Pass 2 maps to the call edge's reason.
#[derive(Clone, Copy)]
struct CallScope<'c> {
    /// The type the calling member belongs to.
    owner: Option<&'c str>,
    /// `o.m()` on a receiver the parser could not type: the member is not
    /// reached through the caller's bases, so they do not narrow the pick
    /// (`super(E, self).m()` starts after `E`).
    untyped_receiver: bool,
    /// A bare `f()` that never reaches a member ([`CallSite::reaches_free_only`]):
    /// the same-file and Global tiers weigh no `Method`, and the heritage
    /// tier does not run. A retry under another name keeps the flag.
    free_only: bool,
    /// The calling declaration: a Rust `self.m()` in an `impl` block the
    /// parser gave no owner reaches that block's own `m`.
    caller: Option<NodeId>,
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
    /// Resolve `qualifier` as a Type (Class / Interface) via Tier 1 → Tier 2 →
    /// Tier 3 (kind-filtered, unique-only), returning the file_path of the
    /// resolved target. Used by Tier 2.5 to scope member lookup to the
    /// qualifier's defining file. Telemetry-silent — internal recursion is
    /// not surfaced in the decision dump.
    fn resolve_qualifier_file(
        &self,
        source_file: &Path,
        qualifier: &str,
        member: &str,
        target: ResolveTarget,
        raw_imports: &[RawImport],
        full_callee: Option<&str>,
    ) -> Option<String> {
        #[cfg(not(windows))]
        let source_file_str = source_file.to_string_lossy();
        #[cfg(windows)]
        let source_file_str =
            std::borrow::Cow::Owned(source_file.to_string_lossy().replace('\\', "/"));

        // Tier 1: same-file qualifier definition. Qualifiers are class /
        // interface / namespace / module names, so filter to Qualifier
        // here — avoids a property named `Logger` winning over `class
        // Logger` in the same file, while still letting `namespace outer
        // { ... }` and inline `mod foo { ... }` serve as the leading
        // segment of `outer::member()` / `foo::member()` calls.
        //
        // Member-presence gate: file-backed `mod auto_ensure;` declares
        // the Module node in the parent file, but the actual members
        // live in `auto_ensure.rs`. Returning the parent file here would
        // short-circuit the downstream member lookup AND block Tier 4's
        // module-file fallback. Only claim Tier 1 when the full chain
        // (qualifier → file → member) resolves in the same file —
        // matches the inline-mod / inline-namespace case and declines
        // for the file-backed stub case, letting later tiers handle it.
        if let Some(id) = self.symbol_table.lookup_in_file_with_kind(
            &source_file_str,
            qualifier,
            ResolveTarget::Qualifier,
        ) {
            if let Some(qf) = self.symbol_table.file_of(id) {
                if self
                    .symbol_table
                    .lookup_in_file_with_kind(qf, member, target)
                    .is_some()
                    && self.rust_type_path_agrees(
                        source_file,
                        &source_file_str,
                        full_callee,
                        qualifier,
                        qf,
                        raw_imports,
                    )
                {
                    return Some(qf.to_string());
                }
            }
        }

        // Tier 2: imported qualifier (matches alias or imported_name; expands
        // specifier via the same L0 candidate enumeration used by the bare-
        // name resolver). Same member-presence gate as Tier 1: the import
        // tells us which file the qualifier lives in, but that file must
        // also contain the member to claim this tier.
        for import in raw_imports {
            let matches_qualifier = match &import.alias {
                Some(alias) => alias == qualifier,
                None => import.imported_name == qualifier,
            };
            if !matches_qualifier
                || full_callee.is_some_and(|callee| {
                    !rust_path_prefix_agrees_with_import(
                        callee,
                        qualifier,
                        &import.source,
                        raw_imports,
                    )
                })
            {
                continue;
            }
            let exported = &import.imported_name;
            // `import * as ns from './a'` / `const ns = require('./a')`: the
            // qualifier is the module, so its file is the lookup scope. A
            // miss ends the search: `ns.f()` never means another file's `f`.
            // A fully qualified `*` import (a wildcard, or a PHP group member)
            // names no module that the qualifier stands for, so it keeps the
            // candidate probe below.
            let language = Language::from_normalized_path(&source_file_str);
            if exported == "*" && import_member_fallback(language) && !fqn_language(language) {
                if let IndexedModule::Unique(file) =
                    self.import_module_file(source_file, &import.source, language)
                {
                    return Some(file.to_string());
                }
                continue;
            }
            if exported == "*" && matches!(language, Language::JavaScript | Language::TypeScript) {
                let mut hit: Option<String> = None;
                self.for_each_candidate(source_file, &import.source, |candidate| {
                    let defines_member = reexport::lookup_export_in_file(
                        &IndexFacts(self),
                        candidate,
                        member,
                        target,
                    )
                    .is_some();
                    if defines_member {
                        hit = Some(candidate.to_string());
                    }
                    !defines_member
                });
                return hit;
            }
            // `use a::m; m::f()`: in Rust `m` may be the module `a::m`, whose
            // own file holds `f`. The parent file declaring `mod m;` can
            // define an `f` of its own, so it is not consulted then; a miss
            // leaves the call to the module tree (Tier 3.5).
            let module_path = if exported.contains("::") {
                exported.clone()
            } else {
                format!("{}::{exported}", import.source)
            };
            if let Some(module_file) = self.rust_module_file(source_file, &module_path) {
                if self
                    .symbol_table
                    .lookup_in_file_with_kind(&module_file, member, target)
                    .is_some()
                {
                    return Some(module_file);
                }
                // The qualifier is that module; the global qualifier lookup
                // below would find `mod m;` in the parent and its own `f`.
                return None;
            }
            // `use other::m; m::f()` names a workspace crate's module by crate
            // (or `[lib]`) name, which `rust_module_file` cannot place. The
            // module tree can; past this point only file-stem guesses remain,
            // and they pick the caller crate's own `m.rs`. Rust callers only:
            // the tree is built for any repo with a Cargo.toml, and a Python
            // `from pkg import m` must not reach a Rust crate named `pkg`. A
            // head that is a module of the caller's own file (`mod utils;
            // use utils::fs;`) is not a crate path.
            let head = module_path.split("::").next().unwrap_or_default();
            if is_rust_source(source_file)
                && self
                    .mod_tree
                    .is_some_and(|tree| tree.names_module(&module_path))
                && self
                    .symbol_table
                    .lookup_in_file_with_kind(&source_file_str, head, ResolveTarget::Qualifier)
                    .is_none()
            {
                // The caller looks `member` up in the returned file, so a
                // renamed re-export (`pub use imp::lookup_impl as lookup;`)
                // would land on an unrelated `lookup` there: the tree must
                // place `member` under its own name.
                return self
                    .mod_tree_resolve(&source_file_str, &format!("{module_path}::{member}"))
                    .filter(|resolved| {
                        resolved.item_name == member
                            && self
                                .symbol_table
                                .lookup_in_file_with_kind(
                                    &resolved.file,
                                    &resolved.item_name,
                                    target,
                                )
                                .is_some()
                    })
                    .map(|resolved| resolved.file);
            }
            let mut hit: Option<String> = None;
            let default_import = ImportRef::from(import).is_es_default();
            self.for_each_candidate(source_file, &import.source, |candidate| {
                // Through the module's exports: an ES module may export the
                // qualifier under another name (`export { Impl as Widget }`).
                let qualifier_present = reexport::imported_in_file(
                    &IndexFacts(self),
                    candidate,
                    exported,
                    default_import,
                    ResolveTarget::Qualifier,
                )
                .is_some();
                let member_present = self
                    .symbol_table
                    .lookup_in_file_with_kind(candidate, member, target)
                    .is_some();
                if qualifier_present && member_present {
                    hit = Some(candidate.to_string());
                    false
                } else {
                    true
                }
            });
            if hit.is_some() {
                return hit;
            }
        }

        // Tier 3: kind-filtered unique global. Language + vendor barriers
        // applied via FileMeta — the same defences as bare-name Tier 3.
        // Member-presence gate identical to Tier 1's: blocks Module's
        // declaration-file from outranking the file-stem fallback when
        // members live elsewhere (`mod foo;` declaration in lib.rs vs.
        // `fn bar()` body in foo.rs).
        let caller_meta = FileMeta::from_path(&source_file_str);
        if let GlobalPick::Unique(id) =
            self.symbol_table
                .lookup_global(qualifier, ResolveTarget::Qualifier, caller_meta, false)
        {
            if let Some(qf) = self.symbol_table.file_of(id) {
                if self
                    .symbol_table
                    .lookup_in_file_with_kind(qf, member, target)
                    .is_some()
                    && self.rust_type_path_agrees(
                        source_file,
                        &source_file_str,
                        full_callee,
                        qualifier,
                        qf,
                        raw_imports,
                    )
                {
                    return Some(qf.to_string());
                }
            }
        }

        // Tier 4: module-file fallback. The qualifier didn't match any Type
        // anywhere, but Rust / Python / similar languages let a *module name*
        // act as a qualifier (`mod auto_ensure;` ↔ `auto_ensure.rs`, `import
        // foo` ↔ `foo.py`). Walk the registered file paths and look for one
        // whose stem matches the qualifier and lives in the caller's crate.
        //
        // Fires only from Tier 2.5 (qualified-call resolution) — heritage
        // resolution passes `None` because parent class names should resolve
        // as Types, not as module files.
        //
        // Two false-positive defences:
        //
        //   1. **Internal-prefix check** — the qualifier's preceding segments
        //      must be empty (bare relative path), or be `crate`/`self`/`super`.
        //      `std::fs::read` becomes ("fs", "read") after split_qualifier;
        //      its preceding segment is `std` → external → declined.
        //
        //   2. **Same-crate prefix** — caller and candidate must share the
        //      same `*/src/` (or `*/tests/`) ancestor. Defends single-crate
        //      repos where the internal-prefix check alone would let a
        //      `std::fs::read` call bind to a workspace `src/fs.rs` — the
        //      preceding segment is `std` so check (1) handles that; check
        //      (2) is the secondary defence if a future extern crate name
        //      manages to slip past check (1).
        let full = full_callee?;
        if !qualifier_prefix_is_internal(full, qualifier) {
            return None;
        }
        let caller_prefix = crate_root_prefix(&source_file_str);
        let mut hit: Option<&str> = None;
        for fp in self.symbol_table.files_by_stem(qualifier) {
            if crate_root_prefix(fp) != caller_prefix {
                continue;
            }
            if hit.is_some() {
                return None;
            }
            hit = Some(fp);
        }
        hit.map(str::to_string)
    }

    /// Python `super().member()`: `member` on the caller class's bases, by
    /// the owner index, never by bare name. Bases are tried in declared order
    /// (a single-pass approximation of the MRO); the first base that owns the
    /// member wins. A base the project cannot see, an ambiguous one, or one
    /// whose ownership is unknown stops the walk: it may own the member and
    /// shadow every later base, so no edge beats a guess.
    ///
    /// A first base that only inherits the member is not enough: C3 puts a
    /// later base ahead of a shared ancestor (`class D(B, C)` with `B(A)`,
    /// `C(A)` reads `D, B, C, A`), so a later base that reaches the member
    /// through another owner, or may do so, leaves no edge.
    #[allow(clippy::too_many_arguments)]
    fn resolve_super_member(
        &self,
        source_file: &Path,
        source_file_str: &str,
        symbol_name: &str,
        member: &str,
        target: ResolveTarget,
        raw_imports: &[RawImport],
        caller_heritage: &[String],
    ) -> Vec<(NodeId, f32)> {
        let mut bases = caller_heritage.iter();
        let mut owner: Option<(&String, NodeId, bool)> = None;
        for base in bases.by_ref() {
            let TypeCandidates::Unique(ty, ty_name) =
                self.super_base_type(source_file, source_file_str, base, raw_imports)
            else {
                break;
            };
            match self.member_ownership(ty, ty_name, member, target) {
                Ownership::Owned { id, inherited } => {
                    owner = Some((base, id, inherited));
                    break;
                }
                Ownership::NotOwned => {}
                Ownership::Unknown => break,
            }
        }
        let owner = owner.filter(|&(_, id, inherited)| {
            !inherited
                || !bases.any(|later| {
                    self.later_base_may_shadow(
                        source_file,
                        source_file_str,
                        later,
                        member,
                        target,
                        raw_imports,
                        id,
                    )
                })
        });
        if let Some((base, id, inherited)) = owner {
            let (tier, conf) = if inherited {
                (DecisionTier::TypeHeritage, ResolutionTier::HeritageScoped)
            } else {
                (DecisionTier::TypeOwned, ResolutionTier::QualifierScoped)
            };
            let conf = conf.base_confidence();
            self.record(
                source_file_str,
                symbol_name,
                Some(base.as_str()),
                tier,
                Some(id),
                0,
                Some(conf),
            );
            return vec![(id, conf)];
        }
        self.record(
            source_file_str,
            symbol_name,
            None,
            DecisionTier::Unresolved,
            None,
            0,
            None,
        );
        Vec::new()
    }

    /// Can `later`, a base declared after the one that inherits `member`
    /// from `owner_member`, come first in the C3 order with another
    /// `member`? A base outside the project cannot derive from a project
    /// class, so it never comes first; an ambiguous base, or one whose
    /// ownership is unknown, may.
    #[allow(clippy::too_many_arguments)]
    fn later_base_may_shadow(
        &self,
        source_file: &Path,
        source_file_str: &str,
        later: &str,
        member: &str,
        target: ResolveTarget,
        raw_imports: &[RawImport],
        owner_member: NodeId,
    ) -> bool {
        match self.super_base_type(source_file, source_file_str, later, raw_imports) {
            TypeCandidates::Unique(ty, ty_name) => {
                match self.member_ownership(ty, ty_name, member, target) {
                    Ownership::Owned { id, .. } => id != owner_member,
                    Ownership::NotOwned => false,
                    Ownership::Unknown => true,
                }
            }
            TypeCandidates::Set(..) => true,
            TypeCandidates::External | TypeCandidates::None => false,
        }
    }

    /// The project type a Python base expression names. A dotted base
    /// (`base.Base` under `import base`) resolves its module through the
    /// qualifier tiers, then the type inside that module; when the module is
    /// in the project but does not declare the type (a package re-export),
    /// the last segment goes through [`Self::type_candidates`]. A module the
    /// project does not hold (`threading.Thread`) gives `None`: a project
    /// `Thread` is not that base.
    fn super_base_type<'q>(
        &self,
        source_file: &Path,
        source_file_str: &str,
        base: &'q str,
        raw_imports: &'q [RawImport],
    ) -> TypeCandidates<'q> {
        let Some((qualifier, type_name)) = split_qualifier(base) else {
            return self.type_candidates(source_file, source_file_str, base, raw_imports);
        };
        let Some(module_file) = self.resolve_qualifier_file(
            source_file,
            qualifier,
            type_name,
            ResolveTarget::Type,
            raw_imports,
            Some(base),
        ) else {
            return TypeCandidates::None;
        };
        match self.symbol_table.lookup_in_file_with_kind(
            &module_file,
            type_name,
            ResolveTarget::Type,
        ) {
            Some(id) => TypeCandidates::Unique(id, type_name),
            None => self.type_candidates(source_file, source_file_str, type_name, raw_imports),
        }
    }

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
        let confidence = |tier: ResolutionTier| tier.base_confidence();
        let candidates = self.type_candidates(source_file, source_file_str, qualifier, raw_imports);
        match type_ladder::member_via_type(
            self.symbol_table,
            candidates,
            source_file_str,
            member,
            target,
            call,
        ) {
            LadderMember::Owned(id) => TypeMember::Edge(
                id,
                DecisionTier::TypeOwned,
                confidence(ResolutionTier::QualifierScoped),
            ),
            LadderMember::Inherited(id) => TypeMember::Edge(
                id,
                DecisionTier::TypeHeritage,
                confidence(ResolutionTier::HeritageScoped),
            ),
            LadderMember::Candidate(id) => TypeMember::Edge(
                id,
                DecisionTier::TypeCandidates,
                confidence(ResolutionTier::Global),
            ),
            LadderMember::NearestCopy(id) => TypeMember::Edge(
                id,
                DecisionTier::NearestCopy,
                confidence(ResolutionTier::NearestCopy),
            ),
            LadderMember::Ambiguous => TypeMember::Ambiguous,
            LadderMember::NoEdge => TypeMember::NoEdge,
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
                    IndexedModule::Unique(file)
                        if !st.owner_path_agrees(&file, fqn_type_path(import)) =>
                    {
                        return TypeCandidates::None;
                    }
                    IndexedModule::Unique(file) => in_file(&file),
                    IndexedModule::Namespace(files) => {
                        let mut hits = files.iter().filter_map(|file| in_file(file));
                        hits.next().filter(|_| hits.next().is_none())
                    }
                    // Java and PHP have always read an import with no
                    // in-repo file as a library type (`use Exception;`), so a
                    // same-named project class elsewhere is not this receiver.
                    // Kotlin imports never matched before, so only a proven
                    // external one stops its global lookup.
                    IndexedModule::Missing
                        if language != Language::Kotlin
                            || self.import_is_external(source_file, import, language) =>
                    {
                        return TypeCandidates::External;
                    }
                    IndexedModule::Missing | IndexedModule::Ambiguous => None,
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

    /// Ladder steps 2 and 3 ([`type_ladder::member_ownership`]).
    fn member_ownership(
        &self,
        ty: NodeId,
        ty_name: &str,
        member: &str,
        target: ResolveTarget,
    ) -> Ownership {
        type_ladder::member_ownership(self.symbol_table, ty, ty_name, member, target)
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

    /// Tier 3.5: attempt module-tree FQN resolution for Rust qualified calls.
    ///
    /// `symbol_name` is the full callee string (e.g.
    /// `"crate::build::orchestrator::build_l2"`).
    /// `member` is its last `::` segment (the item name, same value that
    /// `split_qualifier` already extracted for Tier 2.5).
    ///
    /// Returns the node id of the resolved target, or `None` if the module
    /// tree is absent, the FQN doesn't resolve, or the member isn't found in
    /// the resolved file.
    fn try_module_tree_resolve(
        &self,
        source_file_str: &str,
        symbol_name: &str,
        member: &str,
        target: ResolveTarget,
    ) -> Option<NodeId> {
        let resolved = self.mod_tree_resolve(source_file_str, symbol_name)?;
        self.symbol_table
            .lookup_in_file_with_kind(&resolved.file, &resolved.item_name, target)
            .or_else(|| {
                let bare = member.split('<').next().unwrap_or(member);
                self.symbol_table
                    .lookup_in_file_with_kind(&resolved.file, bare, target)
            })
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
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("a.rs", "new", NodeKind::Method),
        ]);
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
        let st = st_with(&[
            ("a.rs", "A", NodeKind::Class),
            ("a.rs", "new", NodeKind::Method),
        ]);
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
