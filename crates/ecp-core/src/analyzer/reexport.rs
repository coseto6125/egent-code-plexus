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
use crate::graph::{ArchivedFile, ArchivedImportFact, NodeKind};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::hash_map::Entry;
use std::ops::Deref;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// What a bare name in a function body can name as a value
    /// ([`crate::analyzer::types::NameRef`]): a module-level `Const` /
    /// `Variable`, or a `Function` passed or stored. Unlike `Field` it takes
    /// the import tiers: `import { LIMITS }` binds the read. A `Method` or a
    /// `Property` is reached only through the reader's own type.
    Value,
}

impl ResolveTarget {
    pub fn kind_predicate(self) -> fn(NodeKind) -> bool {
        match self {
            Self::Callable => NodeKind::is_callable,
            Self::Method => |kind| kind == NodeKind::Method,
            Self::Type => NodeKind::is_type,
            Self::Qualifier => NodeKind::is_qualifier,
            Self::Field => NodeKind::is_property,
            Self::Value => |kind| {
                matches!(
                    kind,
                    NodeKind::Const | NodeKind::Variable | NodeKind::Function
                )
            },
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
    /// The file the index resolved `source` to, archived for a re-export row
    /// ([`crate::graph::ImportFact::target_file`]). `None` from a parse:
    /// the specifier is probed then.
    pub target: Option<&'a str>,
}

impl<'a> From<&'a RawImport> for ImportRef<'a> {
    fn from(import: &'a RawImport) -> Self {
        Self {
            source: &import.source,
            imported_name: &import.imported_name,
            alias: import.alias.as_deref(),
            binding_kind: import.binding_kind,
            target: None,
        }
    }
}

impl<'a> ImportRef<'a> {
    /// An archived import row; `files` resolves its target file index.
    pub fn from_archived(
        row: &'a ArchivedImportFact,
        pool: &'a [u8],
        files: &'a [ArchivedFile],
    ) -> Self {
        Self {
            source: row.source.resolve(pool),
            imported_name: row.imported_name.resolve(pool),
            alias: row.alias.as_ref().map(|alias| alias.resolve(pool)),
            binding_kind: row.binding_kind.as_ref().map(BindingKind::from),
            target: row
                .target_file
                .as_ref()
                .and_then(|file| files.get(file.to_native() as usize))
                .map(|file| file.path.resolve(pool)),
        }
    }

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

    /// A TS / JS default import, exported or not: it binds its module's
    /// default export.
    pub fn is_es_default(&self) -> bool {
        matches!(
            self.binding_kind,
            Some(BindingKind::EsDefault | BindingKind::EsDefaultReexport)
        )
    }

    /// A TS / JS binding its module exports ([`BindingKind::EsReexport`]).
    pub fn is_es_reexport(&self) -> bool {
        matches!(
            self.binding_kind,
            Some(BindingKind::EsReexport | BindingKind::EsDefaultReexport)
        )
    }
}

/// The ordered imports of one file.
#[derive(Debug, Clone, Copy)]
pub enum FileImports<'a> {
    Parsed(&'a [RawImport]),
    Archived {
        rows: &'a [ArchivedImportFact],
        pool: &'a [u8],
        files: &'a [ArchivedFile],
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
            Self::Archived { rows, pool, files } => ImportRef::from_archived(&rows[i], pool, files),
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

/// The ES-module rule ([`ModuleFacts::is_es_module`]) for `file`, of
/// `language`: [`export_filter_applies`], and the file exports a
/// declaration or has an [`ExportAliasTable`].
pub fn is_es_module(language: Language, file: &str, exports_any: bool, alias_table: bool) -> bool {
    is_ecmascript(language) && !file.ends_with(".d.ts") && (exports_any || alias_table)
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
    /// `export default wrap(f)`: the default export binds no declaration.
    expression_default: bool,
}

impl ExportAliasTable {
    /// The table of a file's `(exported, local)` pairs ([`RawExportAlias`]).
    /// `None` when no pair names both sides and no default is an expression:
    /// the file then exports every declaration under its own name. `default`
    /// with an empty local is an expression default, which alone still makes
    /// the file an ES module.
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
            if exported == "default" && local.is_empty() {
                table.expression_default = true;
                continue;
            }
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
        if table.locals.is_empty() && !table.expression_default {
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
        self.locals.contains_key(name) || (self.expression_default && name == "default")
    }

    /// The local declaration name an import of `name` binds; `None` when it
    /// binds nothing.
    pub fn local<'s>(&'s self, name: &'s str) -> Option<&'s str> {
        match self.locals.get(name) {
            Some(local) => local.as_deref(),
            None if self.hidden.contains(name) => None,
            None if self.expression_default && name == "default" => None,
            None => Some(name),
        }
    }

    /// Whether the default export is an expression (`export default
    /// wrap(f)`), which binds no declaration.
    pub fn exports_expression_default(&self) -> bool {
        self.expression_default
    }
}

/// Where a Python import's module lives in the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PyModule<P> {
    Missing,
    Unique(P),
    Ambiguous,
}

/// A fact a [`ModuleFacts`] side hands out: borrowed from the store it
/// keeps for the whole build, or shared from a memo it fills on demand.
pub enum Fact<'a, T: ?Sized> {
    Borrowed(&'a T),
    Shared(Rc<T>),
}

impl<T: ?Sized> Deref for Fact<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self {
            Self::Borrowed(fact) => fact,
            Self::Shared(fact) => fact,
        }
    }
}

/// What the shared rules read about the repo. Declarations are `u32` node
/// handles in the caller's id space. A file path is repo-relative with
/// forward slashes.
pub trait ModuleFacts {
    /// A module file path [`Self::python_module`] hands back.
    type Path: AsRef<str>;

    /// Whether `file` is indexed, symbols or not.
    fn has_file(&self, file: &str) -> bool;
    /// Files of `language` directly within `directory`.
    fn files_in_directory(&self, _language: Language, _directory: &str) -> Vec<Self::Path> {
        Vec::new()
    }
    /// Files of `language` that declare `namespace`.
    fn namespace_files(&self, _language: Language, _namespace: &str) -> Vec<Self::Path> {
        Vec::new()
    }
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
    fn export_aliases(&self, file: &str) -> Option<Fact<'_, ExportAliasTable>>;
    /// The ordered imports of `file`, with the binding
    /// kinds the parsers marked. `None` when there is no record of them,
    /// which no rule reads as "no imports".
    fn imports(&self, file: &str) -> Option<FileImports<'_>>;
    /// The resolved `export *` modules of the TS / JS `module`
    /// ([`resolve_star_modules`]).
    fn star_modules(&self, module: &str) -> Fact<'_, [Box<str>]>;
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

/// A unique type declared by an unaliased C++ include or Dart file import.
/// Only resolved import files contribute; project-wide namesakes do not.
pub fn imported_file_type<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    language: Language,
    name: &str,
    imports: impl IntoIterator<Item = ImportRef<'i>>,
) -> Option<u32> {
    let mut found = None;
    let mut ambiguous = false;
    for import in imports {
        if import.alias.is_some() {
            continue;
        }
        let mut visit = |source: &str| {
            facts.for_each_candidate(importer, source, &mut |file| {
                for (id, kind) in facts.decls_in_file(file, name) {
                    if kind.is_type() {
                        ambiguous |= found.is_some_and(|previous| previous != id);
                        found = Some(id);
                    }
                }
                !ambiguous
            });
            !ambiguous
        };
        match language {
            Language::Dart => {
                if import.imported_name != import.source
                    || import.source.contains(':')
                    || import.source.starts_with('/')
                {
                    continue;
                }
                if import.source.starts_with('.') {
                    visit(import.source);
                } else {
                    let relative = format!("./{}", import.source);
                    visit(&relative);
                }
            }
            // C++ RawImport strips both `"..."` and `<...>` delimiters.
            // Probe the quoted-include interpretation first, then preserve
            // the existing include-directory interpretation. Different type
            // declarations remain ambiguous instead of picking either path.
            Language::Cpp if !import.source.starts_with('.') && !import.source.starts_with('/') => {
                let relative = format!("./{}", import.source);
                if visit(&relative) {
                    visit(import.source);
                }
            }
            _ => visit(import.source),
        }
        if ambiguous {
            return None;
        }
    }
    found
}

/// Rust import evidence for a return type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustImportedType {
    Unique(u32),
    External,
    Conclusive,
    Unmodeled,
}

/// Resolve direct and glob Rust imports without consulting global names.
pub fn rust_imported_type<'i, F, I>(
    facts: &F,
    importer: &str,
    name: &str,
    imports: I,
) -> RustImportedType
where
    F: ModuleFacts + ?Sized,
    I: Iterator<Item = ImportRef<'i>> + Clone,
{
    // Rust resolves a named binding before considering globs. An explicit
    // external binding is therefore authoritative over a project glob.
    let has_named = imports.clone().any(|import| {
        import.imported_name != "*" && import.alias.unwrap_or(import.imported_name) == name
    });
    let mut found = None;
    let mut ambiguous = false;
    let mut relevant = false;
    let mut external = false;
    let mut forwarded = false;
    for import in imports {
        let is_glob = import.imported_name == "*";
        let binding = import.alias.unwrap_or(import.imported_name);
        if (has_named && (is_glob || binding != name)) || (!has_named && !is_glob) {
            continue;
        }
        relevant = true;
        let declaration = if is_glob { name } else { import.imported_name };
        let mut local = false;
        facts.for_each_candidate(importer, import.source, &mut |file| {
            if !facts.has_file(file) {
                return true;
            }
            local = true;
            let mut declares_here = false;
            for (id, kind) in facts.decls_in_file(file, declaration) {
                if kind.is_type() {
                    declares_here = true;
                    if found.replace(id).is_some_and(|old| old != id) {
                        ambiguous = true;
                        return false;
                    }
                }
            }
            if !declares_here {
                if let Some(nested) = facts.imports(file) {
                    let nested_has_named = nested.iter().any(|import| {
                        import.imported_name != "*"
                            && import.alias.unwrap_or(import.imported_name) == declaration
                    });
                    for nested in nested.iter() {
                        let nested_is_glob = nested.imported_name == "*";
                        let nested_binding = nested.alias.unwrap_or(nested.imported_name);
                        if (nested_has_named && (nested_is_glob || nested_binding != declaration))
                            || (!nested_has_named && !nested_is_glob)
                        {
                            continue;
                        }
                        let mut nested_local = false;
                        facts.for_each_candidate(file, nested.source, &mut |candidate| {
                            if !facts.has_file(candidate) {
                                return true;
                            }
                            nested_local = true;
                            false
                        });
                        if nested_local {
                            // A second local forwarding hop is deliberately
                            // outside this bounded resolver.
                            forwarded = true;
                        } else {
                            external = true;
                        }
                    }
                }
            }
            true
        });
        external |= !local;
    }
    if ambiguous || forwarded {
        RustImportedType::Conclusive
    } else if external {
        RustImportedType::External
    } else if let Some(id) = found {
        RustImportedType::Unique(id)
    } else if relevant {
        RustImportedType::Conclusive
    } else {
        RustImportedType::Unmodeled
    }
}

/// The top-level callable a Go package selector reaches. A Go import's path
/// and its declared package name are independent, so an unaliased selector
/// needs both facts. An explicit alias replaces the package spelling, but
/// remains constrained to the imported directory.
pub fn go_imported_package_member<'a, F, I>(
    facts: &F,
    importer: &str,
    receiver: &str,
    member: &str,
    imports: I,
) -> Option<(u32, &'a str)>
where
    F: ModuleFacts + ?Sized,
    I: IntoIterator<Item = ImportRef<'a>>,
{
    match go_package_lookup(facts, importer, receiver, member, imports) {
        GoPackageLookup::Resolved(target) => target,
        GoPackageLookup::Value | GoPackageLookup::Unmapped => None,
    }
}

/// A selector can name a value, an unmapped package, or a resolved package.
pub enum GoPackageLookup<'a> {
    Value,
    /// Preserve the existing global/nearest-copy heuristic for missing module mappings.
    Unmapped,
    /// A known package with no matching function conclusively resolves to no edge.
    Resolved(Option<(u32, &'a str)>),
}

pub fn go_package_lookup<'a, F, I>(
    facts: &F,
    importer: &str,
    receiver: &str,
    member: &str,
    imports: I,
) -> GoPackageLookup<'a>
where
    F: ModuleFacts + ?Sized,
    I: IntoIterator<Item = ImportRef<'a>>,
{
    let namespace_files = facts.namespace_files(Language::Go, receiver);
    let mut found = None;
    let mut ambiguous = false;
    let mut package = false;
    let mut mapped = false;

    for import in imports {
        let is_alias = import.alias.is_some();
        if is_alias && import.alias != Some(receiver) {
            continue;
        }
        package |= is_alias || import.source.rsplit('/').next() == Some(receiver);
        if !is_alias && namespace_files.is_empty() {
            continue;
        }
        facts.for_each_candidate(importer, import.source, &mut |candidate| {
            let directory = candidate
                .strip_prefix("./")
                .unwrap_or(candidate)
                .trim_end_matches('/');
            for file in facts.files_in_directory(Language::Go, directory) {
                let file = file.as_ref();
                if !is_alias && !namespace_files.iter().any(|path| path.as_ref() == file) {
                    continue;
                }
                mapped = true;
                let mut functions = facts
                    .decls_in_file(file, member)
                    .into_iter()
                    .filter(|&(_, kind)| kind == NodeKind::Function);
                let Some((node, _)) = functions.next() else {
                    continue;
                };
                if functions.next().is_some() {
                    ambiguous = true;
                    return false;
                }
                match found {
                    None => found = Some((node, import.source)),
                    Some((previous, source)) if previous == node && source == import.source => {}
                    Some(_) => {
                        ambiguous = true;
                        return false;
                    }
                }
            }
            !ambiguous
        });
        if ambiguous {
            return GoPackageLookup::Resolved(None);
        }
    }
    if mapped {
        GoPackageLookup::Resolved(found)
    } else if package && !namespace_files.is_empty() {
        GoPackageLookup::Unmapped
    } else if package {
        GoPackageLookup::Resolved(None)
    } else {
        GoPackageLookup::Value
    }
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

/// Every file `import` of `importer` may name, in probe order, until `visit`
/// returns `false`: the file the index resolved it to, when the archive
/// keeps one ([`ImportRef::target`]), then the specifier's candidates.
pub fn for_each_import_candidate<F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    import: &ImportRef<'_>,
    visit: &mut dyn FnMut(&str) -> bool,
) {
    if import.target.is_some_and(|target| !visit(target)) {
        return;
    }
    facts.for_each_candidate(importer, import.source, visit);
}

/// The first TS / JS file `import` names from `importer`: the file module
/// resolution picks, whether or not it declares a given name.
pub fn ecmascript_module<F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    import: &ImportRef<'_>,
) -> Option<Box<str>> {
    let mut found = None;
    for_each_import_candidate(facts, importer, import, &mut |candidate| {
        if is_ecmascript(Language::from_normalized_path(candidate)) && facts.has_file(candidate) {
            found = Some(Box::from(candidate));
            false
        } else {
            true
        }
    });
    found
}

/// The modules the `export *` imports of the TS / JS module `module` name,
/// resolved once per module: a barrel can hold dozens of `export *` lines,
/// and each call through it would otherwise re-probe every specifier.
/// Empty outside TS / JS: a Python star import binds in source order among
/// the module's other imports ([`python_reexport`]).
pub fn resolve_star_modules<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    module: &str,
    imports: impl IntoIterator<Item = ImportRef<'i>>,
) -> Box<[Box<str>]> {
    if !is_ecmascript(Language::from_normalized_path(module)) {
        return Box::default();
    }
    imports
        .into_iter()
        .filter(ImportRef::is_star_export)
        .filter_map(|import| ecmascript_module(facts, module, &import))
        .collect()
}

/// The declaration a default import (`import d from './a'`, local name
/// `local`) binds in module `file`: its default export. A module with no
/// default on record (CommonJS, a script) keeps the by-name lookup of the
/// local name. So does an expression default (`export default
/// withAuth(handler)`): it binds no declaration, and the local name is the
/// evidence of what it wraps. Any private namesake is in reach then.
pub fn default_import_in_file<F: ModuleFacts + ?Sized>(
    facts: &F,
    file: &str,
    local: &str,
    target: ResolveTarget,
) -> Option<u32> {
    lookup_export_in_file(facts, file, "default", target).or_else(|| {
        if facts
            .export_aliases(file)
            .is_some_and(|table| table.exports_expression_default())
        {
            call_in_file(facts, file, local, target)
        } else {
            lookup_export_in_file(facts, file, local, target)
        }
    })
}

/// The declaration an import of `imported` binds in module `file`: a
/// default import binds the default export ([`default_import_in_file`]),
/// any other its export of that name ([`lookup_export_in_file`]).
pub fn imported_in_file<F: ModuleFacts + ?Sized>(
    facts: &F,
    file: &str,
    imported: &str,
    default_import: bool,
    target: ResolveTarget,
) -> Option<u32> {
    if default_import {
        default_import_in_file(facts, file, imported, target)
    } else {
        lookup_export_in_file(facts, file, imported, target)
    }
}

/// What [`named_import`] binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedImport {
    pub decl: u32,
    /// The position of the import that bound it.
    pub position: usize,
    /// The module the import names, when the declaration lives in a module
    /// that one re-exports from ([`ecmascript_reexport`]): the importer
    /// still depends on that barrel.
    pub barrel: Option<Arc<str>>,
}

/// Tier 2 outside the import-member policy: the first import binding `name`
/// whose module, probed through the specifier's candidate files, exports
/// the name it imports. `import d from './a'` binds `a`'s default export
/// ([`default_import_in_file`]). When no candidate exports it and
/// `follow_reexport` allows it, the re-export ([`ecmascript_reexport`]) of
/// the first indexed candidate, the module the specifier names, binds it;
/// for a default import that is the re-export of `default`.
pub fn named_import<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    imports: impl IntoIterator<Item = ImportRef<'i>>,
    name: &str,
    target: ResolveTarget,
    follow_reexport: impl Fn(&str) -> bool,
) -> Option<NamedImport> {
    for (position, import) in imports.into_iter().enumerate() {
        let Some(exported_name) = plain_binding(import.imported_name, import.alias, name) else {
            continue;
        };
        let default_import = import.is_es_default();
        let mut hit: Option<u32> = None;
        let mut module: Option<Arc<str>> = None;
        facts.for_each_candidate(importer, import.source, &mut |candidate| {
            hit = imported_in_file(facts, candidate, exported_name, default_import, target);
            if hit.is_none() && module.is_none() && facts.has_file(candidate) {
                module = Some(Arc::from(candidate));
            }
            hit.is_none()
        });
        if let Some(decl) = hit {
            return Some(NamedImport {
                decl,
                position,
                barrel: None,
            });
        }
        // A specifier that names no indexed file (an npm package) has no
        // re-export to follow.
        let Some(module) = module else {
            continue;
        };
        let reexported = if default_import {
            "default"
        } else {
            exported_name
        };
        if !follow_reexport(reexported) {
            continue;
        }
        if let Some(decl) = ecmascript_reexport(facts, &module, reexported, target) {
            return Some(NamedImport {
                decl,
                position,
                barrel: Some(module),
            });
        }
    }
    None
}

/// `name` re-exported one hop by the TS / JS module `module`: a binding
/// the module exports under the name ([`ImportRef::is_es_reexport`]: an
/// `export { x } from`, or an import its own `export { x }` exports), else
/// the one `export * from` module declaring it. A plain import exports
/// nothing, so it neither binds the name nor shadows a star export. An
/// explicit binding shadows every star export, as in ES modules. Two star
/// modules declaring different targets leave the name unexported in ES
/// modules, so neither binds. A module that re-exports the name again is
/// not followed: one hop, so a cycle ends here.
///
/// A module whose own export clause gives the name under another local
/// (`export { original as name }`, `export default Button`) re-exports
/// nothing from a star module under it: the name binds that local. When the
/// local is an import of the module and no declaration, the import is
/// followed (`import { original } from './impl'; export { original as
/// publicFn }`); otherwise the target module binds only what it exports.
pub fn ecmascript_reexport<F: ModuleFacts + ?Sized>(
    facts: &F,
    module: &str,
    name: &str,
    target: ResolveTarget,
) -> Option<u32> {
    if !is_ecmascript(Language::from_normalized_path(module)) {
        return None;
    }
    if let Some(table) = facts
        .export_aliases(module)
        .filter(|table| table.exports(name))
    {
        let local = table.local(name)?;
        if facts.declares(module, local) {
            return None;
        }
        let import = facts.imports(module)?.iter().find(|import| {
            import.imported_name != "*" && !import.source.is_empty() && import.local_name() == local
        })?;
        return follow_import(facts, module, &import, target);
    }
    let imports = facts.imports(module)?;
    if let Some(import) = imports.iter().find(|import| {
        import.is_es_reexport()
            && import.imported_name != "*"
            && !import.source.is_empty()
            && import.local_name() == name
    }) {
        return follow_import(facts, module, &import, target);
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

/// The declaration the import `import` of `module` binds in the first
/// candidate file that exports it.
fn follow_import<F: ModuleFacts + ?Sized>(
    facts: &F,
    module: &str,
    import: &ImportRef<'_>,
    target: ResolveTarget,
) -> Option<u32> {
    let mut hit = None;
    for_each_import_candidate(facts, module, import, &mut |candidate| {
        hit = imported_in_file(
            facts,
            candidate,
            import.imported_name,
            import.is_es_default(),
            target,
        );
        hit.is_none()
    });
    hit
}

/// `name` re-exported by the Python module `module`: an import of
/// `module` binding `name` makes it an attribute of the module, so
/// `from .core import compute` in `pkg/__init__.py` is `pkg.compute`.
/// Python runs the bindings in source order and the last one wins: an
/// explicit import, or a star import (`from .core import *`) whose module
/// declares the name. A star import skips a `_` name, and a star module
/// with `__all__` exports only what it lists, which the index does not
/// read, so it leaves the name unknown. An import whose module the index
/// does not hold, or does not declare the name, rebinds nothing: it is the
/// missing side of a `try:` / `except ImportError:` pair, an external
/// module, or a second hop.
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
    let mut bound = None;
    for import in facts.imports(module)?.iter() {
        let star = import.is_star_export();
        let member = if star {
            (!name.starts_with('_')).then_some(name)
        } else {
            plain_binding(import.imported_name, import.alias, name)
        };
        let Some(member) = member else {
            continue;
        };
        let file = match facts.python_module(module, import.source) {
            PyModule::Unique(file) => file,
            PyModule::Missing | PyModule::Ambiguous => continue,
        };
        let Some(hit) = call_in_file(facts, file.as_ref(), member, target) else {
            continue;
        };
        bound = Some((!star || !facts.declares(file.as_ref(), "__all__")).then_some(hit));
    }
    bound.flatten()
}

/// [`python_reexport`] for an import of a `language` file: only a Python
/// import binds a name through its package's re-export.
pub fn python_import_reexport<F: ModuleFacts + ?Sized>(
    facts: &F,
    language: Language,
    module: &str,
    name: &str,
    target: ResolveTarget,
) -> Option<u32> {
    (language == Language::Python)
        .then(|| python_reexport(facts, module, name, target))
        .flatten()
}

/// `from pkg.sub import mod; mod.compute()` or `mod.client.compute()`: a
/// from-import of a name its module does not bind imports a submodule, so the
/// call binds as through `import pkg.sub.mod`. `None` on any miss: the call
/// then resolves as the bare member. Returns the declaration and the position
/// of the import that bound it.
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
            let mut receiver_path = receiver.split('/');
            let root = receiver_path.next()?;
            // The cheap gate first: most receivers are objects, whose names
            // no module file or package directory carries.
            if root.is_empty()
                || import.imported_name == "*"
                || import.local_name() != root
                || !facts.has_module_name(Language::Python, import.imported_name)
            {
                return None;
            }
            let package = match facts.python_module(importer, import.source) {
                PyModule::Unique(package) => Some(package),
                PyModule::Ambiguous => return None,
                // A namespace package (no `__init__.py`) is Missing yet holds
                // submodules.
                PyModule::Missing => None,
            };
            let separator = if import.source.bytes().all(|b| b == b'.') {
                ""
            } else if import.source.starts_with('.') {
                "."
            } else {
                "/"
            };
            let nested_separator = if separator.is_empty() { "." } else { separator };
            let mut submodule = [import.source, separator, import.imported_name].concat();
            let mut module = match facts.python_module(importer, &submodule) {
                PyModule::Unique(module) => Some(module),
                // An unindexed package initializer can be a namespace package.
                // Keep walking because a deeper module may still be indexed.
                PyModule::Missing => None,
                PyModule::Ambiguous => return None,
            };
            if package.as_ref().is_some_and(|package| {
                attribute_shadows_submodule(
                    facts,
                    package.as_ref(),
                    import.imported_name,
                    module.as_ref().map(|module| module.as_ref()),
                )
            }) {
                return None;
            }
            for segment in receiver_path {
                if segment.is_empty() {
                    return None;
                }
                submodule.push_str(nested_separator);
                submodule.push_str(segment);
                let next_module = match facts.python_module(importer, &submodule) {
                    PyModule::Unique(module) => Some(module),
                    PyModule::Missing => None,
                    PyModule::Ambiguous => return None,
                };
                if module.as_ref().is_some_and(|module| {
                    attribute_shadows_submodule(
                        facts,
                        module.as_ref(),
                        segment,
                        next_module.as_ref().map(|module| module.as_ref()),
                    )
                }) {
                    return None;
                }
                module = next_module;
            }
            let module = module?;
            let decl = call_in_file(facts, module.as_ref(), member, target)
                .or_else(|| python_reexport(facts, module.as_ref(), member, target))?;
            Some((decl, position))
        })
}

/// Whether a dotted receiver below a `from` import starts with an attribute
/// declared by the imported package. Such an attribute is a value, not an
/// unresolved submodule path.
pub fn python_from_import_declared_attribute<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    receiver: &str,
    imports: impl IntoIterator<Item = ImportRef<'i>>,
) -> bool {
    let Some(root) = receiver.split('/').next() else {
        return false;
    };
    imports.into_iter().any(|import| {
        import.imported_name != "*"
            && import.local_name() == root
            && matches!(
                facts.python_module(importer, import.source),
                PyModule::Unique(module) if facts.declares(module.as_ref(), import.imported_name)
            )
    })
}

/// `from k import K; K.st()`: whether a from-import binds `receiver` to a
/// type its module file declares, so the call names `K.st`. An object, a
/// name the module only re-imports, or a library name keeps the bare member.
pub fn python_imported_type<'i, F: ModuleFacts + ?Sized>(
    facts: &F,
    importer: &str,
    receiver: &str,
    imports: impl IntoIterator<Item = ImportRef<'i>>,
) -> bool {
    let is_type = ResolveTarget::Type.kind_predicate();
    imports.into_iter().any(|import| {
        import.imported_name != "*"
            && import.local_name() == receiver
            && match facts.python_module(importer, import.source) {
                PyModule::Unique(module) => facts
                    .decls_in_file(module.as_ref(), import.imported_name)
                    .iter()
                    .any(|&(_, kind)| is_type(kind)),
                PyModule::Missing | PyModule::Ambiguous => false,
            }
    })
}

/// Whether the Python package module `package` binds `name` to something
/// other than its submodule file `submodule`: that attribute shadows the
/// submodule, as in Python. A declaration binds it, and so does an import:
/// `from .other import name`, `from .client import client`, or a star
/// import of a module that declares it. `from . import name` and
/// `import pkg.name as name` bind the submodule itself.
fn attribute_shadows_submodule<F: ModuleFacts + ?Sized>(
    facts: &F,
    package: &str,
    name: &str,
    submodule: Option<&str>,
) -> bool {
    if facts.declares(package, name) {
        return true;
    }
    let Some(imports) = facts.imports(package) else {
        return false;
    };
    let names_file = |specifier: &str, file: &str| match facts.python_module(package, specifier) {
        PyModule::Unique(found) => found.as_ref() == file,
        PyModule::Missing | PyModule::Ambiguous => false,
    };
    imports.iter().any(|import| {
        if import.is_star_export() {
            return !name.starts_with('_')
                && match facts.python_module(package, import.source) {
                    PyModule::Unique(module) => facts.declares(module.as_ref(), name),
                    PyModule::Missing | PyModule::Ambiguous => false,
                };
        }
        if import.local_name() != name {
            return false;
        }
        let binds_submodule = if import.imported_name == "*" {
            submodule.is_some_and(|submodule| names_file(import.source, submodule))
        } else {
            // `from . import name` binds a namespace child even though it has
            // no indexed initializer file. Other bindings are values until a
            // concrete child proves otherwise.
            import.imported_name == name && names_file(import.source, package)
        };
        !binds_submodule
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
            && ecmascript_module(facts, importer, &import).as_deref() == Some(decl_file)
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
    fn test_export_alias_table_expression_default_binds_no_declaration() {
        let table = ExportAliasTable::build([("config", "config"), ("default", " ")])
            .expect("a named pair makes a table");
        assert!(table.exports_expression_default());
        assert!(table.exports("default"), "no `export *` supplies it");
        assert_eq!(table.local("default"), None);
        assert_eq!(table.local("config"), Some("config"));
        let alone = ExportAliasTable::build([("default", "")])
            .expect("an expression default alone makes an ES module's table");
        assert!(alone.exports("default") && alone.exports_expression_default());
        assert_eq!(alone.local("default"), None);
        assert_eq!(
            alone.local("hidden"),
            Some("hidden"),
            "a private declaration keeps its own name; the export filter decides"
        );
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
