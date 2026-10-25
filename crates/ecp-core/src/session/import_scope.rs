//! The overlay's facts for module discovery, and its memo. The discovery
//! itself is [`crate::analyzer::module_discovery::discover_module`], shared
//! with the full resolver's `import_module_file` (ecp-analyzer
//! `resolution/resolver.rs`).
//!
//! The binding rules themselves live in
//! [`crate::analyzer::import_binding`], shared with the resolver. Module
//! discovery cannot read the resolver's `SymbolTable`; [`ImportScope`]
//! answers the same questions over the base graph's file paths plus the
//! dirty files.

use super::view::OverlayFileInput;
use crate::analyzer::import_binding::{fqn_extension, import_family, may_suppress, module_head};
pub use crate::analyzer::module_discovery::Module;
use crate::analyzer::module_discovery::{discover_module, for_each_layout_candidate, ModuleIndex};
use crate::analyzer::types::RawImport;
use crate::file_category::Language;
use crate::graph::{ArchivedZeroCopyGraph, NodeKind};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::path::Path;

/// One language's indexed files, as the resolver's `SymbolTable` keys them.
#[derive(Default)]
struct LanguageFiles<'a> {
    paths: FxHashSet<&'a str>,
    by_stem: FxHashMap<&'a str, Vec<&'a str>>,
    /// Direct Go children by parent path, preserving Path equality semantics.
    by_directory: FxHashMap<&'a Path, Vec<&'a str>>,
    /// File stems and directory components (`SymbolTable::has_module_name`).
    module_names: FxHashSet<&'a str>,
    /// File → the namespaces it declares (`.`-joined), as the resolver's
    /// `SymbolTable::declares_other_namespace` reads them.
    namespaces: FxHashMap<&'a str, Vec<String>>,
}

impl<'a> LanguageFiles<'a> {
    fn build(
        graph: &'a ArchivedZeroCopyGraph,
        dirty: &'a [OverlayFileInput],
        removed: &FxHashSet<&str>,
        language: Language,
    ) -> Self {
        let mut files = Self::default();
        // A removed path is gone from the worktree. When a dirty file has the
        // same path, the dirty entry below adds it back.
        let base = graph
            .files
            .iter()
            .map(|f| f.path.resolve(&graph.string_pool))
            .filter(|path| !removed.contains(path));
        for path in base.chain(dirty.iter().map(|f| f.rel_path.as_str())) {
            // A dirty file usually exists in the base too; a second entry
            // would make its stem ambiguous.
            if Language::from_normalized_path(path) != language || !files.paths.insert(path) {
                continue;
            }
            let file = Path::new(path);
            if language == Language::Go {
                if let Some(parent) = file.parent() {
                    files.by_directory.entry(parent).or_default().push(path);
                }
            }
            if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
                files.by_stem.entry(stem).or_default().push(path);
                files.module_names.insert(stem);
            }
            if let Some(parent) = file.parent() {
                files.module_names.extend(
                    parent
                        .components()
                        .filter_map(|component| component.as_os_str().to_str()),
                );
            }
        }
        // C# return types also use declared namespace membership.
        if fqn_extension(language).is_none() && language != Language::CSharp {
            return files;
        }
        let fresh: FxHashSet<&str> = dirty.iter().map(|f| f.rel_path.as_str()).collect();
        let base = graph.nodes_by_kind(NodeKind::Namespace).filter_map(|idx| {
            let node = &graph.nodes[idx as usize];
            let path = graph
                .files
                .get(node.file_idx.to_native() as usize)?
                .path
                .resolve(&graph.string_pool);
            (!fresh.contains(path)).then(|| (path, node.name.resolve(&graph.string_pool)))
        });
        let dirty_namespaces = dirty.iter().flat_map(|f| {
            f.symbols
                .iter()
                .filter(|s| s.kind == NodeKind::Namespace)
                .map(move |s| (f.rel_path.as_str(), s.name.as_str()))
        });
        for (path, name) in base.chain(dirty_namespaces) {
            if files.paths.contains(path) {
                let declared = files.namespaces.entry(path).or_default();
                let name = name.replace('\\', ".");
                if !declared.contains(&name) {
                    declared.push(name);
                }
            }
        }
        files
    }

    fn declares_other_namespace(&self, file: &str, expected: &str) -> bool {
        self.namespaces
            .get(file)
            .is_some_and(|declared| declared.iter().all(|ns| ns != expected))
    }
}

/// Module discovery for one overlay build. The file index is built lazily per
/// language, on the first import a policy-language call needs, and each
/// (directory, specifier) answer is cached as the resolver's `module_cache`.
/// The shared import rules read it through `&self` while they probe, so the
/// caches sit in cells.
///
/// `pub` and hidden for `crates/ecp-cli/tests/module_discovery_parity.rs`,
/// which feeds one file set to this and to the resolver; nothing outside
/// the overlay builds one.
#[doc(hidden)]
pub struct ImportScope<'a> {
    graph: &'a ArchivedZeroCopyGraph,
    dirty: &'a [OverlayFileInput],
    dirty_base: &'a FxHashSet<u32>,
    removed: &'a FxHashSet<&'a str>,
    languages: RefCell<FxHashMap<u8, LanguageFiles<'a>>>,
    modules: RefCell<ModuleCache<'a>>,
}

/// language → directory → specifier → module.
type ModuleCache<'a> = FxHashMap<u8, FxHashMap<Box<str>, FxHashMap<Box<str>, Module<&'a str>>>>;

impl<'a> ImportScope<'a> {
    /// The scope over `graph` with the `dirty` files' fresh parses;
    /// `dirty_base` holds the base node ids those parses replace, and
    /// `removed` the base paths gone from the worktree
    /// ([`super::OverlayView::build_with_removed`]). Discovery finds no
    /// removed path unless a dirty file has it.
    pub fn new(
        graph: &'a ArchivedZeroCopyGraph,
        dirty: &'a [OverlayFileInput],
        dirty_base: &'a FxHashSet<u32>,
        removed: &'a FxHashSet<&'a str>,
    ) -> Self {
        Self {
            graph,
            dirty,
            dirty_base,
            removed,
            languages: RefCell::default(),
            modules: RefCell::default(),
        }
    }

    pub(crate) fn dirty(&self) -> &'a [OverlayFileInput] {
        self.dirty
    }

    /// Whether the base path `path` is gone from the worktree. A dirty file
    /// of the same path exists; the caller asks the dirty set first.
    pub(crate) fn is_removed(&self, path: &str) -> bool {
        self.removed.contains(path)
    }

    fn with_files<R>(&self, language: Language, f: impl FnOnce(&LanguageFiles<'a>) -> R) -> R {
        let mut languages = self.languages.borrow_mut();
        let files = languages.entry(language as u8).or_insert_with(|| {
            LanguageFiles::build(self.graph, self.dirty, self.removed, language)
        });
        f(files)
    }

    /// Whether a namespace declared by `target` is also declared by `reader`.
    /// A target without a namespace is the global namespace and remains visible.
    pub(crate) fn namespace_visible_from(
        &self,
        reader: &str,
        target: &str,
        language: Language,
    ) -> bool {
        self.with_files(language, |files| {
            let Some(target_namespaces) = files.namespaces.get(target) else {
                return true;
            };
            files
                .namespaces
                .get(reader)
                .is_some_and(|reader_namespaces| {
                    target_namespaces
                        .iter()
                        .any(|target| reader_namespaces.contains(target))
                })
        })
    }

    /// Whether the `language` files `a` and `b` declare the same namespaces,
    /// as the resolver's `SymbolTable::is_copy` compares them. The resolver
    /// records namespaces for fully qualified languages only, so any other
    /// language agrees without building its file index.
    pub(crate) fn same_namespaces(&self, language: Language, a: &str, b: &str) -> bool {
        fqn_extension(language).is_none()
            || self.with_files(language, |files| {
                files.namespaces.get(a) == files.namespaces.get(b)
            })
    }

    pub(crate) fn has_module_name(&self, language: Language, name: &str) -> bool {
        self.with_files(language, |files| files.module_names.contains(name))
    }

    pub(crate) fn files_in_directory(&self, language: Language, directory: &str) -> Vec<&'a str> {
        self.with_files(language, |files| {
            if language == Language::Go {
                return files
                    .by_directory
                    .get(Path::new(directory))
                    .cloned()
                    .unwrap_or_default();
            }
            files
                .paths
                .iter()
                .copied()
                .filter(|file| Path::new(file).parent() == Some(Path::new(directory)))
                .collect()
        })
    }

    pub(crate) fn namespace_return_type_visible(
        &self,
        file: &str,
        owner: Option<&str>,
        source: &str,
        imports: &[RawImport],
    ) -> bool {
        self.with_files(Language::CSharp, |files| {
            crate::analyzer::type_ladder::namespace_return_type_visible(
                owner,
                files
                    .namespaces
                    .get(file)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
                files
                    .namespaces
                    .get(source)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
                imports,
            )
        })
    }

    pub(crate) fn namespace_type_path_matches(
        &self,
        file: &str,
        owner: Option<&str>,
        path: &str,
    ) -> bool {
        self.with_files(Language::CSharp, |files| {
            crate::analyzer::type_ladder::namespace_type_path_matches(
                owner,
                files
                    .namespaces
                    .get(file)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
                path,
            )
        })
    }

    /// The resolver's `import_is_external`: a Missing import proves its callee
    /// external only when [`may_suppress`] allows it and no language of its
    /// [`import_family`] indexes its head or holds its module.
    pub(crate) fn is_external(
        &self,
        source_file: &str,
        import: &RawImport,
        language: Language,
    ) -> bool {
        let head = module_head(&import.source);
        may_suppress(import, language)
            && import_family(language).all(|family| !self.has_module_name(family, head))
            && import_family(language)
                .all(|family| self.module(source_file, &import.source, family) == Module::Missing)
    }

    /// [`discover_module`] over this build's files, memoized as the
    /// resolver's `import_module_file` memoizes it.
    pub fn module(
        &self,
        source_file: &str,
        specifier: &str,
        language: Language,
    ) -> Module<&'a str> {
        let directory = if specifier.starts_with('.') {
            parent(source_file)
        } else {
            ""
        };
        if let Some(hit) = self
            .modules
            .borrow()
            .get(&(language as u8))
            .and_then(|directories| directories.get(directory))
            .and_then(|specifiers| specifiers.get(specifier))
        {
            return hit.clone();
        }
        let found = self.with_files(language, |files| {
            let index = ScopeIndex {
                files,
                graph: self.graph,
                dirty: self.dirty,
                dirty_base: self.dirty_base,
            };
            discover_module(&index, source_file, specifier, language)
        });
        self.modules
            .borrow_mut()
            .entry(language as u8)
            .or_default()
            .entry(directory.into())
            .or_default()
            .insert(specifier.into(), found.clone());
        found
    }
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// One language's files of an [`ImportScope`], as [`discover_module`] reads
/// them. [`ImportScope::module`] builds it for the language it is asked
/// about, so each method's `language` is that one.
struct ScopeIndex<'s, 'a> {
    files: &'s LanguageFiles<'a>,
    graph: &'a ArchivedZeroCopyGraph,
    dirty: &'a [OverlayFileInput],
    dirty_base: &'s FxHashSet<u32>,
}

impl<'a> ModuleIndex<'a> for ScopeIndex<'_, 'a> {
    fn module_file(&self, _language: Language, path: &str) -> Option<&'a str> {
        self.files.paths.get(path).copied()
    }

    fn module_files_with_stem(
        &self,
        _language: Language,
        stem: &str,
    ) -> impl Iterator<Item = &'a str> {
        self.files.by_stem.get(stem).into_iter().flatten().copied()
    }

    /// As the resolver's `SymbolTable::namespace_files` keys PHP `\` names.
    fn namespace_declarers(&self, _language: Language, name: &str) -> Vec<&'a str> {
        let graph = self.graph;
        let mut found: Vec<&'a str> = Vec::new();
        let mut seen: FxHashSet<&'a str> = FxHashSet::default();
        let backslashed = name.replace('.', "\\");
        let probes =
            std::iter::once(name).chain((backslashed != name).then_some(backslashed.as_str()));
        for probe in probes {
            for idx in graph.nodes_by_name(probe) {
                let node = &graph.nodes[idx as usize];
                if NodeKind::from(&node.kind) != NodeKind::Namespace
                    || self.dirty_base.contains(&idx)
                {
                    continue;
                }
                let Some(path) = graph
                    .files
                    .get(node.file_idx.to_native() as usize)
                    .map(|f| f.path.resolve(&graph.string_pool))
                else {
                    continue;
                };
                if self.files.paths.contains(path) && seen.insert(path) {
                    found.push(path);
                }
            }
        }
        for file in self.dirty {
            let path = file.rel_path.as_str();
            if self.files.paths.contains(path)
                && !seen.contains(path)
                && file
                    .symbols
                    .iter()
                    .any(|s| s.kind == NodeKind::Namespace && s.name.replace('\\', ".") == name)
            {
                found.push(path);
            }
        }
        found
    }

    /// A dirty file answers from its fresh parse, never from its stale base
    /// nodes.
    fn module_declares(&self, file: &str, name: &str) -> bool {
        let graph = self.graph;
        match self.dirty.iter().find(|d| d.rel_path == file) {
            Some(fresh) => fresh.symbols.iter().any(|s| s.name == name),
            None => graph.nodes_by_name(name).any(|idx| {
                graph
                    .files
                    .get(graph.nodes[idx as usize].file_idx.to_native() as usize)
                    .is_some_and(|f| f.path.resolve(&graph.string_pool) == file)
            }),
        }
    }

    fn declares_other_namespace(&self, file: &str, namespace: &str) -> bool {
        self.files.declares_other_namespace(file, namespace)
    }

    /// The layout candidates alone: the overlay reads no path alias here,
    /// as it read none before discovery was shared.
    fn for_each_module_candidate(
        &self,
        importer: &str,
        specifier: &str,
        visit: impl FnMut(&str) -> bool,
    ) {
        for_each_layout_candidate(importer, specifier, visit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "manual release-mode directory lookup benchmark"]
    fn test_files_in_directory_large_go_workload_reports_timing() {
        let total = std::time::Instant::now();
        let mut fixture = crate::graph_fixture::GraphFixture::new();
        for index in 0..15_000 {
            fixture.file(&format!("pkg/{index}/file.go"));
        }
        let bytes = fixture.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        let dirty_base = FxHashSet::default();
        let removed = FxHashSet::default();
        let scope = ImportScope::new(graph, &[], &dirty_base, &removed);
        let cold = std::time::Instant::now();
        assert_eq!(
            scope.files_in_directory(Language::Go, "pkg/1"),
            ["pkg/1/file.go"]
        );
        let cold_seconds = cold.elapsed().as_secs_f64();
        let warm = std::time::Instant::now();
        let mut found = 0;
        for index in 0..3_000 {
            found += std::hint::black_box(
                scope.files_in_directory(Language::Go, &format!("pkg/{}", index % 15)),
            )
            .len();
        }
        assert_eq!(found, 3_000);
        println!("directory_profile cold_seconds={cold_seconds:.6} warm_seconds={:.6} total_seconds={:.6} found={found}",
            warm.elapsed().as_secs_f64(), total.elapsed().as_secs_f64());
    }
}
