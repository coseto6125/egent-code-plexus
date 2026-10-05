//! Module discovery for the overlay's import-member tier, mirroring the full
//! resolver's `import_module_file` (ecp-analyzer `resolution/resolver.rs`).
//!
//! The binding rules themselves live in
//! [`crate::analyzer::import_binding`], shared with the resolver. Module
//! discovery cannot reuse the resolver's `SymbolTable`; [`ImportScope`]
//! replays it over the base graph's file paths plus the dirty files.

use super::view::OverlayFileInput;
use crate::analyzer::import_binding::{fqn_extension, import_family, may_suppress, module_head};
use crate::analyzer::types::RawImport;
use crate::file_category::Language;
use crate::graph::{ArchivedZeroCopyGraph, NodeKind};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;

/// Where an import's module lives in the repo.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Module<'a> {
    Missing,
    Unique(&'a str),
    Ambiguous,
    /// Files declaring the imported namespace that define the imported
    /// member (all of them when the import names the namespace itself).
    Namespace(Vec<&'a str>),
}

/// One language's indexed files, as the resolver's `SymbolTable` keys them.
#[derive(Default)]
struct LanguageFiles<'a> {
    paths: FxHashSet<&'a str>,
    by_stem: FxHashMap<&'a str, Vec<&'a str>>,
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
        language: Language,
    ) -> Self {
        let mut files = Self::default();
        let base = graph
            .files
            .iter()
            .map(|f| f.path.resolve(&graph.string_pool));
        for path in base.chain(dirty.iter().map(|f| f.rel_path.as_str())) {
            // A dirty file usually exists in the base too; a second entry
            // would make its stem ambiguous.
            if Language::from_normalized_path(path) != language || !files.paths.insert(path) {
                continue;
            }
            let file = Path::new(path);
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
pub(crate) struct ImportScope<'a> {
    graph: &'a ArchivedZeroCopyGraph,
    dirty: &'a [OverlayFileInput],
    dirty_base: &'a FxHashSet<u32>,
    languages: FxHashMap<u8, LanguageFiles<'a>>,
    modules: FxHashMap<(u8, &'a str, &'a str), Module<'a>>,
}

impl<'a> ImportScope<'a> {
    pub(crate) fn new(
        graph: &'a ArchivedZeroCopyGraph,
        dirty: &'a [OverlayFileInput],
        dirty_base: &'a FxHashSet<u32>,
    ) -> Self {
        Self {
            graph,
            dirty,
            dirty_base,
            languages: FxHashMap::default(),
            modules: FxHashMap::default(),
        }
    }

    pub(crate) fn dirty(&self) -> &'a [OverlayFileInput] {
        self.dirty
    }

    pub(crate) fn has_module_name(&mut self, language: Language, name: &str) -> bool {
        let (graph, dirty) = (self.graph, self.dirty);
        self.languages
            .entry(language as u8)
            .or_insert_with(|| LanguageFiles::build(graph, dirty, language))
            .module_names
            .contains(name)
    }

    /// The resolver's `import_is_external`: a Missing import proves its callee
    /// external only when [`may_suppress`] allows it and no language of its
    /// [`import_family`] indexes its head or holds its module.
    pub(crate) fn is_external(
        &mut self,
        source_file: &'a str,
        import: &'a RawImport,
        language: Language,
    ) -> bool {
        let head = module_head(&import.source);
        may_suppress(import, language)
            && import_family(language).all(|family| !self.has_module_name(family, head))
            && import_family(language)
                .all(|family| self.module(source_file, &import.source, family) == Module::Missing)
    }

    /// The resolver's `import_module_file`: the module `specifier` names from
    /// `source_file`, without guessing its member.
    pub(crate) fn module(
        &mut self,
        source_file: &'a str,
        specifier: &'a str,
        language: Language,
    ) -> Module<'a> {
        let directory = if specifier.starts_with('.') {
            parent(source_file)
        } else {
            ""
        };
        let key = (language as u8, directory, specifier);
        if let Some(hit) = self.modules.get(&key) {
            return hit.clone();
        }
        let (graph, dirty) = (self.graph, self.dirty);
        let files = self
            .languages
            .entry(language as u8)
            .or_insert_with(|| LanguageFiles::build(graph, dirty, language));
        let found = match fqn_extension(language) {
            Some(extension) => {
                fqn_module(files, graph, dirty, self.dirty_base, specifier, extension)
            }
            None => python_module(files, source_file, specifier),
        };
        self.modules.insert(key, found.clone());
        found
    }
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The resolver's Python import candidates: the specifier's base path
/// (`for_each_specifier_candidate`), then `python::spec::module_candidates`.
fn python_candidates(source_file: &str, specifier: &str) -> Vec<String> {
    let base = if specifier.starts_with('.') {
        // PEP 328: N leading dots walk N-1 packages up from the caller.
        let dots = specifier.bytes().take_while(|&b| b == b'.').count();
        let dotted = specifier[dots..].trim_start_matches('.').replace('.', "/");
        let dotted = dotted.trim_start_matches('/');
        let mut dir = parent(source_file);
        for _ in 1..dots {
            dir = parent(dir);
        }
        match (dir.is_empty(), dotted.is_empty()) {
            (true, _) => dotted.to_string(),
            (false, true) => dir.to_string(),
            (false, false) => format!("{dir}/{dotted}"),
        }
    } else if !specifier.contains("://") && !specifier.is_empty() {
        specifier.trim_end_matches('/').to_string()
    } else {
        return Vec::new();
    };
    // Python loads packages before modules, and implementation files before stubs.
    ["/__init__.py", "/__init__.pyi", ".py", ".pyi"]
        .iter()
        .map(|suffix| {
            if base.is_empty() {
                suffix.trim_start_matches('/').to_string()
            } else {
                format!("{base}{suffix}")
            }
        })
        .collect()
}

fn first_indexed<'a>(files: &LanguageFiles<'a>, candidates: &[String]) -> Option<&'a str> {
    candidates
        .iter()
        .find_map(|candidate| files.paths.get(candidate.as_str()).copied())
}

/// The resolver's `discover_import_module` for Python: the specifier's own
/// module, else one implicit source root (`src/pkg/util.py` for `pkg.util`).
fn python_module<'a>(files: &LanguageFiles<'a>, source_file: &str, specifier: &str) -> Module<'a> {
    let candidates = python_candidates(source_file, specifier);
    let exact = first_indexed(files, &candidates);
    if exact.is_some() || specifier.starts_with('.') {
        return exact.map_or(Module::Missing, Module::Unique);
    }
    let mut found: Option<(&str, &'a str)> = None;
    for candidate in &candidates {
        let Some(stem) = Path::new(candidate).file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        for &file in files.by_stem.get(stem).into_iter().flatten() {
            let Some(root) = file
                .strip_suffix(candidate.as_str())
                .filter(|prefix| prefix.ends_with('/'))
            else {
                continue;
            };
            // A directory already indexed as a module is a package, not an
            // implicit source root for an absolute import.
            let root_module = python_candidates(source_file, root.trim_end_matches('/'));
            if first_indexed(files, &root_module).is_some() {
                continue;
            }
            match found {
                Some((previous, _)) if previous != root => return Module::Ambiguous,
                Some(_) => {}
                None => found = Some((root, file)),
            }
        }
    }
    found.map_or(Module::Missing, |(_, file)| Module::Unique(file))
}

/// The resolver's `discover_fqn_module`: the longest prefix of the import
/// path that names a declared namespace or a conventionally placed file.
fn fqn_module<'a>(
    files: &LanguageFiles<'a>,
    graph: &'a ArchivedZeroCopyGraph,
    dirty: &'a [OverlayFileInput],
    dirty_base: &FxHashSet<u32>,
    specifier: &str,
    extension: &str,
) -> Module<'a> {
    let normalized = specifier.trim_start_matches('\\').replace('\\', ".");
    let mut name = normalized.as_str();
    loop {
        let declaring = namespace_files(files, graph, dirty, dirty_base, name);
        if !declaring.is_empty() {
            let member = normalized
                .strip_prefix(name)
                .unwrap_or("")
                .trim_start_matches('.')
                .split('.')
                .next()
                .unwrap_or("");
            return Module::Namespace(
                declaring
                    .into_iter()
                    .filter(|file| member.is_empty() || defines(graph, dirty, file, member))
                    .collect(),
            );
        }
        let candidate = format!("{}.{extension}", name.replace('.', "/"));
        let stem = name.rsplit('.').next().unwrap_or(name);
        let namespace = name.rsplit_once('.').map_or("", |(outer, _)| outer);
        let placed: Vec<&'a str> = files
            .by_stem
            .get(stem)
            .into_iter()
            .flatten()
            .copied()
            .filter(|file| {
                (*file == candidate
                    || file
                        .strip_suffix(candidate.as_str())
                        .is_some_and(|root| root.ends_with('/')))
                    && !files.declares_other_namespace(file, namespace)
            })
            .collect();
        match placed.as_slice() {
            [] => {}
            [file] => return Module::Unique(file),
            _ => return Module::Ambiguous,
        }
        let Some((outer, _)) = name.rsplit_once('.') else {
            break;
        };
        name = outer;
    }
    Module::Missing
}

/// Files of this language declaring namespace `name` (`.`-separated, as the
/// resolver's `SymbolTable::namespace_files` keys PHP `\` names).
fn namespace_files<'a>(
    files: &LanguageFiles<'a>,
    graph: &'a ArchivedZeroCopyGraph,
    dirty: &'a [OverlayFileInput],
    dirty_base: &FxHashSet<u32>,
    name: &str,
) -> Vec<&'a str> {
    let mut found: Vec<&'a str> = Vec::new();
    let backslashed = name.replace('.', "\\");
    let probes = std::iter::once(name).chain((backslashed != name).then_some(backslashed.as_str()));
    for probe in probes {
        for idx in graph.nodes_by_name(probe) {
            let node = &graph.nodes[idx as usize];
            if NodeKind::from(&node.kind) != NodeKind::Namespace || dirty_base.contains(&idx) {
                continue;
            }
            let Some(path) = graph
                .files
                .get(node.file_idx.to_native() as usize)
                .map(|f| f.path.resolve(&graph.string_pool))
            else {
                continue;
            };
            if files.paths.contains(path) && !found.contains(&path) {
                found.push(path);
            }
        }
    }
    for file in dirty {
        let path = file.rel_path.as_str();
        if files.paths.contains(path)
            && !found.contains(&path)
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

/// Does `file` declare anything named `member`? A dirty file answers from its
/// fresh parse, never from its stale base nodes.
fn defines(
    graph: &ArchivedZeroCopyGraph,
    dirty: &[OverlayFileInput],
    file: &str,
    member: &str,
) -> bool {
    match dirty.iter().find(|d| d.rel_path == file) {
        Some(fresh) => fresh.symbols.iter().any(|s| s.name == member),
        None => graph.nodes_by_name(member).any(|idx| {
            graph
                .files
                .get(graph.nodes[idx as usize].file_idx.to_native() as usize)
                .is_some_and(|f| f.path.resolve(&graph.string_pool) == file)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_candidates_relative_and_absolute_match_resolver_order() {
        assert_eq!(
            python_candidates("a/b/app.py", "..x.y"),
            [
                "a/x/y/__init__.py",
                "a/x/y/__init__.pyi",
                "a/x/y.py",
                "a/x/y.pyi"
            ]
        );
        assert_eq!(
            python_candidates("app.py", "."),
            ["__init__.py", "__init__.pyi", ".py", ".pyi"]
        );
        assert_eq!(python_candidates("app.py", "pkg/util")[2], "pkg/util.py");
        assert!(python_candidates("app.py", "").is_empty());
    }
}
