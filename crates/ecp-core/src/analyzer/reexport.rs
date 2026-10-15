//! The import tier's module rules, shared by the index-time resolver
//! (ecp-analyzer `resolution/resolver.rs`) and the query-time overlay
//! (`session/view.rs`), so the two cannot drift: which declaration an ES
//! module import binds (export eligibility and export aliases), the one-hop
//! re-export through a Python package or a TS / JS barrel, the Python
//! submodule from-import, the ES-module privacy rule of the global tier,
//! and the candidate files an import specifier probes.
//!
//! Each side supplies facts through [`ModuleFacts`]: the index reads its
//! `SymbolTable`, the overlay reads the graph archive with each dirty
//! file's fresh parse in place of its archived facts. No rule lives in an
//! adapter.

use crate::analyzer::import_binding::plain_binding;
use crate::analyzer::rust_paths::{is_rust_module_root, is_rust_source, rust_module_path_base};
use crate::analyzer::types::{BindingKind, RawImport};
use crate::file_category::Language;
use crate::graph::{ArchivedImportFact, NodeKind};
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::path::Path;

/// Edge kinds the resolver resolves towards. Constrains Tier-3 fallback so a
/// bare callee like `format` never resolves to a Variable/Const that happens
/// to share the name.
///
/// `Qualifier` is the leading-segment lookup used by `resolve_qualifier_file`
/// — it accepts Type (Class/Struct/Enum/Typedef/Trait/Interface) plus
/// Namespace (C++ / C# / PHP) and Module (Rust inline `mod`). Without
/// these, every qualified call whose leading segment isn't a class /
/// struct / enum / typedef / trait / interface drops at Tier 2.5 since
/// the qualifier kind doesn't pass `is_type` — falling to the bare-name
/// Tier 3 which rejects ultra-common member names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveTarget {
    Callable,
    /// A Rust method-syntax call (`x.f()`): only a `Method` can answer it,
    /// never a free `fn` of the same name.
    Method,
    Type,
    Qualifier,
    /// A struct/class field, for `ReadsField` edge resolution. Filters to
    /// `NodeKind::is_property`.
    Field,
}

impl ResolveTarget {
    pub fn kind_predicate(self) -> fn(NodeKind) -> bool {
        match self {
            Self::Callable => NodeKind::is_callable,
            Self::Method => |kind| kind == NodeKind::Method,
            Self::Type => NodeKind::is_type,
            Self::Qualifier => NodeKind::is_qualifier,
            Self::Field => NodeKind::is_property,
        }
    }
}

/// `SymbolTable::lookup_call_in_file` over the declarations of one name in
/// one file, in source order: the first of `target`'s kinds. Method syntax
/// narrows the result, not the field: a same-named free fn in the file makes
/// the name ambiguous, so `Path::new(".").join(..)` on a std type never
/// binds the file's `Local::join`.
pub fn pick_call(
    decls: impl Iterator<Item = (u32, NodeKind)> + Clone,
    target: ResolveTarget,
) -> Option<u32> {
    if target == ResolveTarget::Method
        && decls
            .clone()
            .any(|(_, kind)| kind.is_callable() && kind != NodeKind::Method)
    {
        return None;
    }
    let predicate = target.kind_predicate();
    decls
        .into_iter()
        .find(|&(_, kind)| predicate(kind))
        .map(|(id, _)| id)
}

/// One import of a file, borrowed from a parse ([`RawImport`]) or from the
/// graph archive ([`crate::graph::ImportFact`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportRef<'a> {
    pub source: &'a str,
    pub imported_name: &'a str,
    pub alias: Option<&'a str>,
    pub binding_kind: Option<BindingKind>,
}

impl<'a> From<&'a RawImport> for ImportRef<'a> {
    fn from(import: &'a RawImport) -> Self {
        Self {
            source: &import.source,
            imported_name: &import.imported_name,
            alias: import.alias.as_deref(),
            binding_kind: import.binding_kind,
        }
    }
}

impl<'a> ImportRef<'a> {
    /// The name the import binds in its file.
    pub fn local_name(&self) -> &'a str {
        self.alias.unwrap_or(self.imported_name)
    }

    /// `export * from 'lib'` / Python `from lib import *`: the parsers emit
    /// it as a `*` import with no local binding, which no import of the
    /// file itself resolves through.
    pub fn is_star_export(&self) -> bool {
        self.imported_name == "*" && self.alias.is_none()
    }
}

/// The ordered imports of one file.
#[derive(Debug, Clone, Copy)]
pub enum FileImports<'a> {
    Parsed(&'a [RawImport]),
    Archived {
        rows: &'a [ArchivedImportFact],
        pool: &'a [u8],
    },
}

impl<'a> FileImports<'a> {
    pub fn iter(self) -> impl Iterator<Item = ImportRef<'a>> {
        let len = match self {
            Self::Parsed(imports) => imports.len(),
            Self::Archived { rows, .. } => rows.len(),
        };
        (0..len).map(move |i| match self {
            Self::Parsed(imports) => ImportRef::from(&imports[i]),
            Self::Archived { rows, pool } => rows[i].to_ref(pool),
        })
    }
}

/// Whether exports decide what another file may import from `file`: a TS /
/// JS file other than a `.d.ts`, whose declarations TS may export
/// implicitly. Other languages bind imports by their own rules.
pub fn export_filter_applies(file: &str) -> bool {
    is_ecmascript(Language::from_normalized_path(file)) && !file.ends_with(".d.ts")
}

pub fn is_ecmascript(language: Language) -> bool {
    matches!(language, Language::TypeScript | Language::JavaScript)
}

/// The export names of one ES module, recorded because one of them differs
/// from the declaration it binds.
#[derive(Debug, Clone, Default)]
pub struct ExportAliasTable {
    /// Export name → the local declaration name it binds. `None` when the
    /// module exports the name for two declarations, which ES rejects.
    locals: FxHashMap<Box<str>, Option<Box<str>>>,
    /// Local names the module exports only under other names: an import of
    /// one binds nothing.
    hidden: FxHashSet<Box<str>>,
}

impl ExportAliasTable {
    /// The table of a file's `(exported, local)` pairs ([`RawExportAlias`]).
    /// `None` when no pair names both sides: the file then exports every
    /// declaration under its own name.
    ///
    /// [`RawExportAlias`]: crate::analyzer::types::RawExportAlias
    pub fn build<'p, I>(pairs: I) -> Option<Self>
    where
        I: IntoIterator<Item = (&'p str, &'p str)>,
        I::IntoIter: Clone,
    {
        let pairs = pairs.into_iter();
        let mut table = Self::default();
        for (exported, local) in pairs.clone() {
            let (exported, local) = (exported.trim(), local.trim());
            if exported.is_empty() || local.is_empty() {
                continue;
            }
            match table.locals.entry(exported.into()) {
                Entry::Vacant(slot) => {
                    slot.insert(Some(local.into()));
                }
                Entry::Occupied(mut slot) => {
                    if slot.get().as_deref() != Some(local) {
                        *slot.get_mut() = None;
                    }
                }
            }
        }
        if table.locals.is_empty() {
            return None;
        }
        table.hidden = pairs
            .map(|(_, local)| local.trim())
            .filter(|local| !local.is_empty() && !table.locals.contains_key(*local))
            .map(Box::<str>::from)
            .collect();
        Some(table)
    }

    /// Whether the module itself exports `name` under an export name of its
    /// own (`export { g as name }`, `export default`): no `export *` of the
    /// module can then supply the name.
    pub fn exports(&self, name: &str) -> bool {
        self.locals.contains_key(name)
    }

    /// The local declaration name an import of `name` binds; `None` when it
    /// binds nothing.
    pub fn local<'s>(&'s self, name: &'s str) -> Option<&'s str> {
        match self.locals.get(name) {
            Some(local) => local.as_deref(),
            None if self.hidden.contains(name) => None,
            None => Some(name),
        }
    }
}

/// Where a Python import's module lives in the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PyModule<P> {
    Missing,
    Unique(P),
    Ambiguous,
}

/// What the shared rules read about the repo. Declarations are `u32` node
/// handles in the caller's id space. A file path is repo-relative with
/// forward slashes.
pub trait ModuleFacts {
    /// A module file path [`Self::python_module`] hands back.
    type Path: AsRef<str>;

    /// Whether `file` is indexed, symbols or not.
    fn has_file(&self, file: &str) -> bool;
    /// Whether a file stem or directory of `language` is named `name`.
    fn has_module_name(&self, language: Language, name: &str) -> bool;
    /// Whether `file` declares `name`, any kind.
    fn declares(&self, file: &str, name: &str) -> bool;
    /// `file`'s declarations of `name`, in source order, with their kinds.
    /// Only the first declaration of each uid counts, as in the index.
    fn decls_in_file(&self, file: &str, name: &str) -> Vec<(u32, NodeKind)>;
    /// Whether the declaration `decl` is exported by its file.
    fn is_exported(&self, decl: u32) -> bool;
    /// Whether `file` is an ES module: [`export_filter_applies`] and it
    /// exports a declaration or has an [`ExportAliasTable`].
    fn is_es_module(&self, file: &str) -> bool;
    /// `file`'s [`ExportAliasTable`], `None` when it has none.
    fn export_aliases(&self, file: &str) -> Option<Cow<'_, ExportAliasTable>>;
    /// The ordered imports of a Python / TS / JS `file`. `None` when there
    /// is no record of them, which no rule reads as "no imports".
    fn imports(&self, file: &str) -> Option<FileImports<'_>>;
    /// The resolved `export *` / `from m import *` modules of `module`
    /// ([`resolve_star_modules`]).
    fn star_modules(&self, module: &str) -> Cow<'_, [Box<str>]>;
    /// The resolver's `import_module_file` for a Python specifier.
    fn python_module(&self, importer: &str, specifier: &str) -> PyModule<Self::Path>;
    /// Every file `specifier` may name from `importer`, in probe order,
    /// until `visit` returns `false`.
    fn for_each_candidate(
        &self,
        importer: &str,
        specifier: &str,
        visit: &mut dyn FnMut(&str) -> bool,
    );
}

/// `SymbolTable::lookup_call_in_file` through `facts`.
pub fn call_in_file<F: ModuleFacts + ?Sized>(
    facts: &F,
    file: &str,
    name: &str,
    target: ResolveTarget,
) -> Option<u32> {
    pick_call(facts.decls_in_file(file, name).into_iter(), target)
}

/// The declaration an import of `name` from module `file` binds. In an ES
/// module that is an exported one, found under the declaration name the
/// export name stands for (`export { g as f }` binds `f` to `g`): a
/// module-private or nested namesake is out of the importer's reach, so a
/// module that does not export the name answers `None`. Any other module
/// keeps [`call_in_file`].
pub fn lookup_export_in_file<F: ModuleFacts + ?Sized>(
    facts: &F,
    file: &str,
    name: &str,
    target: ResolveTarget,
) -> Option<u32> {
    if !facts.is_es_module(file) {
        return call_in_file(facts, file, name, target);
    }
    let aliases = facts.export_aliases(file);
    let local = match aliases.as_deref() {
        Some(table) => table.local(name)?,
        None => name,
    };
    let predicate = target.kind_predicate();
    facts
        .decls_in_file(file, local)
        .into_iter()
        .find(|&(decl, kind)| facts.is_exported(decl) && predicate(kind))
        .map(|(decl, _)| decl)
}

/// The first TS / JS file `specifier` names from `importer`: the file
/// module resolution picks, whether or not it declares a given name.
pub fn ecmascript_module<F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    specifier: &str,
) -> Option<Box<str>> {
    let mut found = None;
    facts.for_each_candidate(importer, specifier, &mut |candidate| {
        if is_ecmascript(Language::from_normalized_path(candidate)) && facts.has_file(candidate) {
            found = Some(Box::from(candidate));
            false
        } else {
            true
        }
    });
    found
}

/// The modules the star imports of `module` name, resolved once per module:
/// a barrel can hold dozens of `export *` lines, and each call through it
/// would otherwise re-probe every specifier. Empty outside Python / TS / JS.
pub fn resolve_star_modules<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    module: &str,
    imports: impl IntoIterator<Item = ImportRef<'i>>,
) -> Box<[Box<str>]> {
    let language = Language::from_normalized_path(module);
    let stars = imports.into_iter().filter(ImportRef::is_star_export);
    if is_ecmascript(language) {
        stars
            .filter_map(|import| ecmascript_module(facts, module, import.source))
            .collect()
    } else if language == Language::Python {
        stars
            .filter_map(|import| match facts.python_module(module, import.source) {
                PyModule::Unique(file) => Some(Box::<str>::from(file.as_ref())),
                PyModule::Missing | PyModule::Ambiguous => None,
            })
            .collect()
    } else {
        Box::default()
    }
}

/// Tier 2 outside the import-member policy: the first import binding `name`
/// whose module, probed through the specifier's candidate files, exports
/// the name it imports. `import d from './a'` binds `a`'s default export; a
/// module with no default on record (CommonJS, a script) keeps the by-name
/// lookup of the local name. When the module exports nothing under that
/// name and `follow_reexport` allows it, the module's re-export
/// ([`ecmascript_hop`]) binds it. Returns the declaration and the position
/// of the import that bound it.
pub fn named_import<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    imports: impl IntoIterator<Item = ImportRef<'i>>,
    name: &str,
    target: ResolveTarget,
    follow_reexport: impl Fn(&str) -> bool,
) -> Option<(u32, usize)> {
    for (position, import) in imports.into_iter().enumerate() {
        let Some(exported_name) = plain_binding(import.imported_name, import.alias, name) else {
            continue;
        };
        let default_import = import.binding_kind == Some(BindingKind::EsDefault);
        let mut hit: Option<u32> = None;
        facts.for_each_candidate(importer, import.source, &mut |candidate| {
            hit = default_import
                .then(|| lookup_export_in_file(facts, candidate, "default", target))
                .flatten()
                .or_else(|| lookup_export_in_file(facts, candidate, exported_name, target));
            hit.is_none()
        });
        // An npm package's export reaches nothing through an in-repo
        // re-export, so it skips the second probe.
        if hit.is_none() && follow_reexport(exported_name) {
            hit = ecmascript_hop(facts, importer, import.source, exported_name, target);
        }
        if let Some(decl) = hit {
            return Some((decl, position));
        }
    }
    None
}

/// `name` through the TS / JS module `specifier` names from `importer`
/// when that module does not declare it: the re-export of the first
/// indexed candidate file ([`ecmascript_reexport`]).
pub fn ecmascript_hop<F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    specifier: &str,
    name: &str,
    target: ResolveTarget,
) -> Option<u32> {
    let mut hit = None;
    facts.for_each_candidate(importer, specifier, &mut |candidate| {
        if !facts.has_file(candidate) {
            return true;
        }
        hit = ecmascript_reexport(facts, candidate, name, target);
        false
    });
    hit
}

/// `name` re-exported one hop by the TS / JS module `module`: an
/// `export { x } from` (or an import, which the parsers emit alike)
/// binding the name, else the one `export * from` module declaring it.
/// An explicit binding shadows every star export, as in ES modules. Two
/// star modules declaring different targets leave the name unexported
/// in ES modules, so neither binds. A module that re-exports the name
/// again is not followed: one hop, so a cycle ends here. A module whose
/// own export clause gives the name (`export { g as name }`) re-exports
/// nothing under it, and the target module binds only what it exports.
pub fn ecmascript_reexport<F: ModuleFacts + ?Sized>(
    facts: &F,
    module: &str,
    name: &str,
    target: ResolveTarget,
) -> Option<u32> {
    if facts
        .export_aliases(module)
        .is_some_and(|table| table.exports(name))
    {
        return None;
    }
    if !is_ecmascript(Language::from_normalized_path(module)) {
        return None;
    }
    let imports = facts.imports(module)?;
    if let Some(import) = imports.iter().find(|import| {
        import.imported_name != "*" && !import.source.is_empty() && import.local_name() == name
    }) {
        let mut hit = None;
        facts.for_each_candidate(module, import.source, &mut |candidate| {
            hit = lookup_export_in_file(facts, candidate, import.imported_name, target);
            hit.is_none()
        });
        return hit;
    }
    // `export *` never re-exports a module's default export.
    if name == "default" {
        return None;
    }
    let stars = facts.star_modules(module);
    let mut hits = stars
        .iter()
        .filter_map(|file| lookup_export_in_file(facts, file, name, target));
    let first = hits.next()?;
    hits.all(|other| other == first).then_some(first)
}

/// `name` re-exported by the Python module `module`: an import of
/// `module` binding `name` makes it an attribute of the module, so
/// `from .core import compute` in `pkg/__init__.py` is `pkg.compute`.
/// The first such import whose module declares it wins, which keeps a
/// `try:` / `except ImportError:` pair on the module that exists.
///
/// When no import binds the name, a star import (`from .core import *`)
/// does: the one star module that declares it, as in ES modules. Two
/// that declare different targets bind nothing, and so does a `_` name,
/// which a star import skips. A star module with `__all__` exports only
/// what it lists, which the index does not read, so it binds nothing.
///
/// One hop: a module that re-exports the name again is not followed, so
/// a longer chain keeps the general tiers instead of a guessed target,
/// and a cycle ends here.
pub fn python_reexport<F: ModuleFacts + ?Sized>(
    facts: &F,
    module: &str,
    name: &str,
    target: ResolveTarget,
) -> Option<u32> {
    if Language::from_normalized_path(module) != Language::Python {
        return None;
    }
    let imports = facts.imports(module)?;
    let mut bound = false;
    let explicit = imports.iter().find_map(|import| {
        let member = plain_binding(import.imported_name, import.alias, name)?;
        bound = true;
        match facts.python_module(module, import.source) {
            PyModule::Unique(file) => call_in_file(facts, file.as_ref(), member, target),
            PyModule::Missing | PyModule::Ambiguous => None,
        }
    });
    if bound || name.starts_with('_') {
        return explicit;
    }
    let stars = facts.star_modules(module);
    let mut hits = stars.iter().filter_map(|file| {
        let hit = call_in_file(facts, file, name, target)?;
        Some((!facts.declares(file, "__all__")).then_some(hit))
    });
    let first = hits.next()??;
    hits.all(|other| other == Some(first)).then_some(first)
}

/// `from pkg.sub import mod; mod.compute()`: a from-import of a name its
/// module does not declare imports the submodule `pkg.sub.mod`, so the
/// call binds as through `import pkg.sub.mod as mod`. `None` on any miss:
/// the call then resolves as the bare member. Returns the declaration and
/// the position of the import that bound it.
pub fn python_submodule_member<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    (receiver, member): (&str, &str),
    imports: impl IntoIterator<Item = ImportRef<'i>>,
    target: ResolveTarget,
) -> Option<(u32, usize)> {
    imports
        .into_iter()
        .enumerate()
        .find_map(|(position, import)| {
            // The cheap gate first: most receivers are objects, whose names
            // no module file or package directory carries.
            if import.imported_name == "*"
                || import.local_name() != receiver
                || !facts.has_module_name(Language::Python, import.imported_name)
            {
                return None;
            }
            let shadowed = match facts.python_module(importer, import.source) {
                // The module's own `mod` is an attribute, which shadows the
                // submodule.
                PyModule::Unique(module) => facts.declares(module.as_ref(), import.imported_name),
                PyModule::Ambiguous => true,
                // A namespace package (no `__init__.py`) is Missing yet holds
                // submodules.
                PyModule::Missing => false,
            };
            if shadowed {
                return None;
            }
            let separator = if import.source.bytes().all(|b| b == b'.') {
                ""
            } else if import.source.starts_with('.') {
                "."
            } else {
                "/"
            };
            let submodule = [import.source, separator, import.imported_name].concat();
            let decl = match facts.python_module(importer, &submodule) {
                PyModule::Unique(module) => call_in_file(facts, module.as_ref(), member, target)
                    .or_else(|| python_reexport(facts, module.as_ref(), member, target)),
                PyModule::Missing | PyModule::Ambiguous => None,
            }?;
            Some((decl, position))
        })
}

/// Whether a TS / JS import binding `name` names the ES module that
/// declares the global tier's pick (in `decl_file`). The import tier found
/// no export of that name there, so the declaration is private to its
/// module and out of the importer's reach: the global tier must not bind
/// it.
pub fn import_hides<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    name: &str,
    imports: impl IntoIterator<Item = ImportRef<'i>>,
    decl_file: &str,
) -> bool {
    if !is_ecmascript(Language::from_normalized_path(importer)) || !facts.is_es_module(decl_file) {
        return false;
    }
    imports.into_iter().any(|import| {
        import.imported_name != "*"
            && import.local_name() == name
            && ecmascript_module(facts, importer, import.source).as_deref() == Some(decl_file)
    })
}

/// Extensions probed during candidate enumeration (covers every language
/// whose parser is wired into ecp-analyzer).
pub const EXT_CANDIDATES: &[&str] = &[
    ".ts", ".tsx", ".jsx", ".js", ".mjs", ".cjs", ".py", ".pyi", ".rs", ".go", ".java", ".kt",
    ".rb", ".php", ".cs", ".swift", ".dart", ".sol", ".sql",
];

/// Package-style suffixes — a directory acting as a module.
pub const INDEX_SUFFIXES: &[&str] = &[
    "/index.ts",
    "/index.tsx",
    "/index.js",
    "/index.jsx",
    "/__init__.py",
    "/mod.rs",
    "/lib.rs",
    "/main.rs",
];

/// Enumerate every file key that `specifier` could plausibly map to from
/// `source_file`, invoking `visit` for each. The closure returns `true` to
/// keep going, `false` to short-circuit.
///
/// * **Verbatim specifier** is visited first so behavior is a strict
///   superset of pre-L0.
/// * **Aliases**: `expand_aliases` hands each project alias expansion of
///   the specifier (TS `tsconfig.json` paths, etc.) to the callback it
///   receives; each goes through the extension / index suffix probing.
/// * **Relative** (`./x`, `../x`, `.x`, `..x.y`): joined against the
///   source file's parent directory, accounting for Python-style
///   multi-dot prefixes and dotted submodule paths (`from .a.b import C`).
/// * **Both relative and absolute**: try common extensions (`.ts .tsx .py
///   .rs ...`) and package-style suffixes (`/index.ts`, `/__init__.py`,
///   `/mod.rs`).
///
/// A single `String` buffer is reused across all suffixed probes, so
/// total allocations per call are bounded by O(1) heap activity once
/// the closure starts running. This matters on the resolver hot path
/// where Tier 2 fires once per (callsite, heritage, type, framework-ref).
pub fn for_each_specifier_candidate<F>(
    source_file: &Path,
    specifier: &str,
    expand_aliases: impl FnOnce(&mut dyn FnMut(&str) -> bool),
    mut visit: F,
) where
    F: FnMut(&str) -> bool,
{
    if !visit(specifier) {
        return;
    }

    // Alias expansion runs *before* relative resolution: aliased specifiers
    // like `@/utils` never look like a relative path and would otherwise
    // fall straight through to the Tier-3 global fallback.
    let mut stopped = false;
    expand_aliases(&mut |expanded| {
        if probe_with_suffixes(expanded, &mut visit) {
            true
        } else {
            stopped = true;
            false
        }
    });
    if stopped {
        return;
    }

    let dir = source_file.parent().unwrap_or(Path::new(""));
    let mut rust_module = false;
    let base_path: Option<std::path::PathBuf> = if let Some(rest) = specifier.strip_prefix("./") {
        Some(dir.join(rest))
    } else if specifier.starts_with("../") {
        let mut p = dir.to_path_buf();
        let mut s = specifier;
        while let Some(rest) = s.strip_prefix("../") {
            p = p.parent().unwrap_or(Path::new("")).to_path_buf();
            s = rest;
        }
        Some(p.join(s))
    } else if let Some(rust_base) = rust_module_path_base(source_file, specifier) {
        // Rust `use` path: `crate::output`, `super::a::b`, `self::x`, or a
        // workspace-internal `<crate>::mod::item` head. Map to the caller
        // crate's `src/<segments>` so Tier-2 import resolution can pin a
        // bare `emit()` call to its declaring module instead of dropping to
        // the Tier-3 same-name ambiguity cap. Without this, `ecp impact`
        // undercounts callers of any common-named cross-module Rust fn
        // (the #100-122 incident: `emit` reported 1 of 21 callers).
        rust_module = true;
        Some(rust_base)
    } else if specifier.starts_with('.') {
        // Python-style relative: count leading dots, then a dotted submodule
        // path. `.foo` from `src/pkg/x.py` → `src/pkg/foo`. `..foo.bar` →
        // walk parent once, then `foo/bar`. `...foo` → walk two parents,
        // then `foo` (PEP 328: N dots = walk N-1 packages).
        let dots = specifier.bytes().take_while(|&b| b == b'.').count();
        let rest = &specifier[dots..];
        // Strip any leftover leading `.` (e.g. `....foo` past the dot count)
        // and the implicit leading `/` that Path::join would otherwise treat
        // as absolute and discard the base.
        let dotted = rest.trim_start_matches('.').replace('.', "/");
        let dotted = dotted.trim_start_matches('/');
        let mut p = dir.to_path_buf();
        for _ in 1..dots {
            p = p.parent().unwrap_or(Path::new("")).to_path_buf();
        }
        Some(if dotted.is_empty() { p } else { p.join(dotted) })
    } else {
        None
    };

    let base = if let Some(b) = base_path {
        let b_str = b.to_string_lossy().replace('\\', "/");
        Some(
            b_str
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string(),
        )
    } else if !specifier.contains("://") && !specifier.is_empty() {
        // Absolute-but-pathlike: `a/b` style. Still worth probing.
        Some(specifier.trim_end_matches('/').to_string())
    } else {
        None
    };

    let Some(base) = base else { return };

    if !rust_module {
        probe_with_suffixes(&base, &mut visit);
    } else if probe_rust_module(&base, &mut visit) {
        if let Some(fallback) = rust_self_fallback_base(source_file, specifier) {
            let fallback = fallback.to_string_lossy().replace('\\', "/");
            probe_rust_module(fallback.trim_start_matches("./"), &mut visit);
        }
    }
}

/// [`probe_with_suffixes`] for a Rust module path, which only a Rust file
/// can declare: `base.rs`, then the directory module files. A same-stem
/// `base.ts` beside the module is another language's file, not this module.
/// `lib.rs` / `main.rs` cover `crate` itself (`use crate::X` maps to `src`).
pub fn probe_rust_module<F>(base: &str, visit: &mut F) -> bool
where
    F: FnMut(&str) -> bool,
{
    [".rs", "/mod.rs", "/lib.rs", "/main.rs"]
        .iter()
        .all(|suffix| visit(&format!("{base}{suffix}")))
}

/// `self::rest` from an ordinary module file, anchored at the file's own
/// directory: where a crate root keeps its children. A root named by a
/// Cargo `[lib] path` / `[[bin]] path` (e.g. `src/api.rs`) is not
/// recognisable from its file name, so it is probed only after the module's
/// own child directory came up empty.
fn rust_self_fallback_base(source_file: &Path, specifier: &str) -> Option<std::path::PathBuf> {
    let rest = specifier.strip_prefix("self::")?;
    if !is_rust_source(source_file) || is_rust_module_root(source_file) {
        return None;
    }
    let dir = source_file.parent()?;
    Some(
        rest.split("::")
            .filter(|s| !s.is_empty())
            .fold(dir.to_path_buf(), |p, seg| p.join(seg)),
    )
}

/// Probe `base`, then `base + ext` for each known extension, then
/// `base + index_suffix` for each known index suffix. Returns `false`
/// if the visitor short-circuited, `true` if all probes were exhausted
/// without finding a hit.
fn probe_with_suffixes<F>(base: &str, visit: &mut F) -> bool
where
    F: FnMut(&str) -> bool,
{
    if !visit(base) {
        return false;
    }
    let mut buf = String::with_capacity(base.len() + 16);
    for ext in EXT_CANDIDATES {
        buf.clear();
        buf.push_str(base);
        buf.push_str(ext);
        if !visit(&buf) {
            return false;
        }
    }
    for suf in INDEX_SUFFIXES {
        buf.clear();
        buf.push_str(base);
        buf.push_str(if base.is_empty() {
            suf.trim_start_matches('/')
        } else {
            suf
        });
        if !visit(&buf) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_alias_table_alias_conflict_and_hidden_returns_resolver_answers() {
        let table =
            ExportAliasTable::build([("f", "g"), ("d", "x"), ("d", "y"), ("k", "k"), (" ", "z")])
                .expect("named pairs make a table");
        assert_eq!(table.local("f"), Some("g"), "an alias binds its local");
        assert_eq!(
            table.local("d"),
            None,
            "two locals for one export bind none"
        );
        assert_eq!(
            table.local("g"),
            None,
            "a local exported only as `f` is hidden"
        );
        assert_eq!(table.local("k"), Some("k"));
        assert_eq!(
            table.local("other"),
            Some("other"),
            "unlisted names keep their own"
        );
        assert!(table.exports("f") && !table.exports("g"));
        // `z` pairs with a blank export name: no entry, yet it is hidden.
        assert_eq!(table.local("z"), None);
    }

    #[test]
    fn test_export_alias_table_empty_or_blank_pairs_returns_none() {
        assert!(ExportAliasTable::build(std::iter::empty::<(&str, &str)>()).is_none());
        assert!(ExportAliasTable::build([("", "g"), ("f", "  ")]).is_none());
    }

    #[test]
    fn test_pick_call_method_target_with_free_fn_namesake_returns_none() {
        let decls = [(1, NodeKind::Function), (2, NodeKind::Method)];
        assert_eq!(pick_call(decls.into_iter(), ResolveTarget::Method), None);
        assert_eq!(
            pick_call(decls.into_iter(), ResolveTarget::Callable),
            Some(1)
        );
        let methods = [(3, NodeKind::Property), (4, NodeKind::Method)];
        assert_eq!(
            pick_call(methods.into_iter(), ResolveTarget::Method),
            Some(4)
        );
        assert_eq!(pick_call(std::iter::empty(), ResolveTarget::Callable), None);
    }

    #[test]
    fn test_export_filter_applies_ts_js_but_not_dts_or_python() {
        assert!(export_filter_applies("src/a.ts"));
        assert!(export_filter_applies("src/a.mjs"));
        assert!(!export_filter_applies("src/a.d.ts"));
        assert!(!export_filter_applies("pkg/__init__.py"));
    }
}
