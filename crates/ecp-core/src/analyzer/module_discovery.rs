//! Module discovery: the file an import specifier names, without guessing
//! its member. The index-time resolver (ecp-analyzer
//! `Resolver::import_module_file`) and the session overlay
//! (`session/import_scope.rs`) both run [`discover_module`] (B §4.2, row
//! B6a); each answers the questions through its own [`ModuleIndex`] and
//! keeps its own memo.
//!
//! Discovery is a standalone step, not a tier of [`crate::analyzer::ladder`]:
//! it picks no declaration. The import-member tier, the qualifier tier, the
//! receiver-typing ladder and the Python re-export rules all read its answer.

use crate::analyzer::import_binding::fqn_extension;
use crate::analyzer::reexport::{for_each_specifier_candidate, EXT_CANDIDATES, INDEX_SUFFIXES};
use crate::file_category::Language;
use std::path::Path;
use std::sync::Arc;

/// Where an import's module lives in the repo.
#[derive(Clone, Debug, PartialEq)]
pub enum Module<P> {
    Missing,
    Unique(P),
    Ambiguous,
    /// Files declaring the imported namespace that define the imported
    /// member (all of them when the import names the namespace itself).
    /// Shared, so a memo hit does not copy a large namespace.
    Namespace(Arc<[P]>),
}

impl<P> Module<P> {
    /// The same answer with each file path converted by `f`.
    pub fn map<Q>(&self, mut f: impl FnMut(&P) -> Q) -> Module<Q> {
        match self {
            Self::Missing => Module::Missing,
            Self::Unique(file) => Module::Unique(f(file)),
            Self::Ambiguous => Module::Ambiguous,
            Self::Namespace(files) => Module::Namespace(files.iter().map(f).collect()),
        }
    }
}

/// What module discovery reads about the indexed files. Each method answers
/// a question; [`discover_module`] decides. A path is repo-relative with
/// forward slashes, borrowed from the index for `'a`.
pub trait ModuleIndex<'a> {
    /// The indexed `language` file at exactly `path`.
    fn module_file(&self, language: Language, path: &str) -> Option<&'a str>;
    /// The indexed `language` files whose file stem is `stem`, in any order.
    fn module_files_with_stem(
        &self,
        language: Language,
        stem: &str,
    ) -> impl Iterator<Item = &'a str>;
    /// The indexed `language` files that declare the namespace `name`
    /// (`.`-joined, PHP `\` included).
    fn namespace_declarers(&self, language: Language, name: &str) -> Vec<&'a str>;
    /// Whether `file` declares anything named `name`.
    fn module_declares(&self, file: &str, name: &str) -> bool;
    /// Whether `file` declares a namespace and none of them is `namespace`
    /// (`.`-joined; empty for the global namespace).
    fn declares_other_namespace(&self, file: &str, namespace: &str) -> bool;
    /// Every path `specifier` may name from `importer`, in probe order,
    /// until `visit` returns `false`: the verbatim specifier first.
    fn for_each_module_candidate(
        &self,
        importer: &str,
        specifier: &str,
        visit: impl FnMut(&str) -> bool,
    );
    /// The root directory of `importer`'s C2 scope (R-A4b); `None` when it
    /// has none. When several source roots or namespace files hold an
    /// absolute import's module, the one under this root is the module.
    fn importer_scope_root(&self, _importer: &str) -> Option<&str> {
        None
    }
}

/// `file` is under the directory `root`, by whole segments; every file is
/// under the repo root `""`.
fn under_root(file: &str, root: &str) -> bool {
    root.is_empty()
        || file
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Of several candidates, the single one under the importer's scope root
/// `root`; `None` when no root is given or not exactly one is under it.
fn sole_under_root<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
    root: Option<&str>,
) -> Option<&'a str> {
    let root = root?;
    let mut under = candidates.into_iter().filter(|file| under_root(file, root));
    match (under.next(), under.next()) {
        (Some(file), None) => Some(file),
        _ => None,
    }
}

/// The module `specifier` names from `importer`. Only files of `language`
/// count: the caller's, or for the suppression check another language of its
/// import family. Relative specifiers stay anchored to their caller; an
/// absolute module path may sit under one source-root prefix, and two
/// matching roots are ambiguous.
pub fn discover_module<'a, I: ModuleIndex<'a>>(
    index: &I,
    importer: &str,
    specifier: &str,
    language: Language,
) -> Module<&'a str> {
    match fqn_extension(language) {
        Some(extension) => fqn_module(index, importer, specifier, language, extension),
        None => path_module(index, importer, specifier, language),
    }
}

/// The importer's scope root when it can decide between candidates: the
/// repo root holds every candidate, so it decides nothing.
fn deciding_scope_root<'i, 'a, I: ModuleIndex<'a>>(
    index: &'i I,
    importer: &str,
) -> Option<&'i str> {
    index
        .importer_scope_root(importer)
        .filter(|root| !root.is_empty())
}

/// The specifier's own module, else one implicit source root
/// (`src/pkg/util.py` for `pkg/util`). Of several roots, the one under the
/// importer's scope root is the module: two `pyproject.toml` projects that
/// each hold a package `app` each import their own.
fn path_module<'a, I: ModuleIndex<'a>>(
    index: &I,
    importer: &str,
    specifier: &str,
    language: Language,
) -> Module<&'a str> {
    let exact = first_indexed(index, importer, specifier, language);
    if exact.is_some() || specifier.starts_with('.') {
        return exact.map_or(Module::Missing, Module::Unique);
    }
    let scope_root = deciding_scope_root(index, importer);
    // Each distinct root with its first file, in candidate order.
    let mut roots: Vec<(&'a str, &'a str)> = Vec::new();
    for_each_import_candidate(index, importer, specifier, language, |candidate| {
        if Language::from_normalized_path(candidate) != language {
            return true;
        }
        let Some(stem) = Path::new(candidate).file_stem().and_then(|s| s.to_str()) else {
            return true;
        };
        for file in index.module_files_with_stem(language, stem) {
            let Some(root) = file
                .strip_suffix(candidate)
                .filter(|prefix| prefix.ends_with('/'))
            else {
                continue;
            };
            // A directory already indexed as a module is a package, not an
            // implicit source root for an absolute import.
            if first_indexed(index, importer, root.trim_end_matches('/'), language).is_some() {
                continue;
            }
            // Candidate order already selected this root's package or
            // implementation.
            if roots.iter().any(|&(seen, _)| seen == root) {
                continue;
            }
            roots.push((root, file));
            // Two roots and no scope root to decide: ambiguous already.
            if roots.len() > 1 && scope_root.is_none() {
                return false;
            }
        }
        true
    });
    match roots.as_slice() {
        [] => Module::Missing,
        [(_, file)] => Module::Unique(file),
        _ => {
            let mut own = roots
                .iter()
                .filter(|&&(root, _)| scope_root.is_some_and(|scope| under_root(root, scope)));
            match (own.next(), own.next()) {
                (Some(&(_, file)), None) => Module::Unique(file),
                _ => Module::Ambiguous,
            }
        }
    }
}

/// The longest prefix of a fully qualified import path that names a
/// declared namespace or a conventionally placed file. Of several files
/// that declare the imported member or sit at its place, the one under the
/// importer's scope root is the module: two C# projects or two Composer
/// packages that each declare it each import their own.
fn fqn_module<'a, I: ModuleIndex<'a>>(
    index: &I,
    importer: &str,
    specifier: &str,
    language: Language,
    extension: &str,
) -> Module<&'a str> {
    let scope_root = deciding_scope_root(index, importer);
    let normalized = specifier.trim_start_matches('\\').replace('\\', ".");
    let mut name = normalized.as_str();
    loop {
        let declaring = index.namespace_declarers(language, name);
        if !declaring.is_empty() {
            let member = normalized
                .strip_prefix(name)
                .unwrap_or("")
                .trim_start_matches('.')
                .split('.')
                .next()
                .unwrap_or("");
            let files: Vec<&'a str> = declaring
                .into_iter()
                .filter(|file| member.is_empty() || index.module_declares(file, member))
                .collect();
            // An import of the namespace itself names every declaring file.
            let own = (!member.is_empty() && files.len() > 1)
                .then(|| sole_under_root(files.iter().copied(), scope_root))
                .flatten();
            return Module::Namespace(match own {
                Some(file) => vec![file].into(),
                None => files.into(),
            });
        }
        let candidate = format!("{}.{extension}", name.replace('.', "/"));
        let stem = name.rsplit('.').next().unwrap_or(name);
        // A placed file that declares another namespace is a namesake:
        // `use InvalidArgumentException;` is not `Lib\Testing`'s class.
        let namespace = name.rsplit_once('.').map_or("", |(outer, _)| outer);
        let placed = {
            let placed: Vec<&'a str> = index
                .module_files_with_stem(language, stem)
                .filter(|file| {
                    (*file == candidate
                        || file
                            .strip_suffix(candidate.as_str())
                            .is_some_and(|root| root.ends_with('/')))
                        && !index.declares_other_namespace(file, namespace)
                })
                .collect();
            match placed.as_slice() {
                [] => None,
                [file] => Some(Module::Unique(*file)),
                _ => Some(
                    sole_under_root(placed.iter().copied(), scope_root)
                        .map_or(Module::Ambiguous, Module::Unique),
                ),
            }
        };
        if let Some(module) = placed {
            return module;
        }
        let Some((outer, _)) = name.rsplit_once('.') else {
            break;
        };
        name = outer;
    }
    Module::Missing
}

fn first_indexed<'a, I: ModuleIndex<'a>>(
    index: &I,
    importer: &str,
    specifier: &str,
    language: Language,
) -> Option<&'a str> {
    let mut found = None;
    for_each_import_candidate(index, importer, specifier, language, |candidate| {
        found = index.module_file(language, candidate);
        found.is_none()
    });
    found
}

/// The index's candidates, except that for Python each base path after the
/// verbatim specifier (an alias expansion, then the specifier's own path) is
/// probed in [`python_candidates`] order, and the shared probe suffixes the
/// index appends to a base are skipped. A base that misses hands over to the
/// next one: a project-name alias whose `src/<name>` expansion comes first
/// must not hide a flat-layout `<name>/` package.
fn for_each_import_candidate<'a, I: ModuleIndex<'a>>(
    index: &I,
    importer: &str,
    specifier: &str,
    language: Language,
    mut visit: impl FnMut(&str) -> bool,
) {
    if language != Language::Python {
        index.for_each_module_candidate(importer, specifier, visit);
        return;
    }
    let mut verbatim = true;
    let mut base = String::new();
    index.for_each_module_candidate(importer, specifier, |candidate| {
        if verbatim {
            verbatim = false;
            return true;
        }
        let probe_of_base = !base.is_empty()
            && candidate.strip_prefix(base.as_str()).is_some_and(|rest| {
                rest.is_empty() || EXT_CANDIDATES.contains(&rest) || INDEX_SUFFIXES.contains(&rest)
            });
        if probe_of_base {
            return true;
        }
        base.clear();
        base.push_str(candidate);
        python_candidates(candidate, &mut visit)
    });
}

/// Python loads packages before modules, and implementation files before
/// stubs. Returns `false` when `visit` stopped the probe.
fn python_candidates(base: &str, visit: &mut impl FnMut(&str) -> bool) -> bool {
    let mut candidate = String::with_capacity(base.len() + 13);
    for suffix in ["/__init__.py", "/__init__.pyi", ".py", ".pyi"] {
        candidate.clear();
        if base.is_empty() {
            candidate.push_str(suffix.trim_start_matches('/'));
        } else {
            candidate.push_str(base);
            candidate.push_str(suffix);
        }
        if !visit(&candidate) {
            return false;
        }
    }
    true
}

/// [`ModuleIndex::for_each_module_candidate`] for an index without project
/// path aliases or a Rust module tree: the shared specifier candidates.
pub fn for_each_layout_candidate(importer: &str, specifier: &str, visit: impl FnMut(&str) -> bool) {
    for_each_specifier_candidate(Path::new(importer), specifier, |_| {}, visit);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file list, the namespaces its files declare and their declarations,
    /// and the scope root every importer reads.
    #[derive(Default)]
    struct Files {
        paths: Vec<&'static str>,
        namespaces: Vec<(&'static str, &'static str)>,
        decls: Vec<(&'static str, &'static str)>,
        scope_root: Option<&'static str>,
        /// One project alias `pattern/*` -> each `replacement/*`.
        alias: Option<(&'static str, &'static [&'static str])>,
    }

    impl ModuleIndex<'static> for Files {
        fn module_file(&self, language: Language, path: &str) -> Option<&'static str> {
            self.paths
                .iter()
                .copied()
                .find(|&file| file == path && Language::from_normalized_path(file) == language)
        }

        fn module_files_with_stem(
            &self,
            language: Language,
            stem: &str,
        ) -> impl Iterator<Item = &'static str> {
            self.paths.iter().copied().filter(move |file| {
                Language::from_normalized_path(file) == language
                    && Path::new(file).file_stem().and_then(|s| s.to_str()) == Some(stem)
            })
        }

        fn namespace_declarers(&self, language: Language, name: &str) -> Vec<&'static str> {
            self.namespaces
                .iter()
                .filter(|&&(file, namespace)| {
                    namespace == name && Language::from_normalized_path(file) == language
                })
                .map(|&(file, _)| file)
                .collect()
        }

        fn module_declares(&self, file: &str, name: &str) -> bool {
            self.decls.iter().any(|&(f, n)| f == file && n == name)
        }

        fn declares_other_namespace(&self, file: &str, namespace: &str) -> bool {
            let mut declared = self
                .namespaces
                .iter()
                .filter(|&&(f, _)| f == file)
                .peekable();
            declared.peek().is_some() && declared.all(|&(_, ns)| ns != namespace)
        }

        fn for_each_module_candidate(
            &self,
            importer: &str,
            specifier: &str,
            visit: impl FnMut(&str) -> bool,
        ) {
            let alias = self.alias;
            for_each_specifier_candidate(
                Path::new(importer),
                specifier,
                |expanded| {
                    let Some((pattern, replacements)) = alias else {
                        return;
                    };
                    let Some(rest) = specifier
                        .strip_prefix(pattern)
                        .and_then(|r| r.strip_prefix('/'))
                    else {
                        return;
                    };
                    for replacement in replacements {
                        if !expanded(&format!("{replacement}/{rest}")) {
                            return;
                        }
                    }
                },
                visit,
            );
        }

        fn importer_scope_root(&self, _importer: &str) -> Option<&str> {
            self.scope_root
        }
    }

    fn files(paths: &[&'static str]) -> Files {
        Files {
            paths: paths.to_vec(),
            ..Files::default()
        }
    }

    const PYTHON: &[&str] = &[
        "pkg/__init__.py",
        "pkg/util.py",
        "pkg/sub/__init__.py",
        "pkg/sub/mod.py",
        "src/lib/tool.py",
        "a/dup/x.py",
        "b/dup/x.py",
    ];

    fn python(importer: &str, specifier: &str) -> Module<&'static str> {
        discover_module(&files(PYTHON), importer, specifier, Language::Python)
    }

    #[test]
    fn test_python_candidates_empty_base_drops_the_slash() {
        let mut seen = Vec::new();
        python_candidates("", &mut |candidate| {
            seen.push(candidate.to_string());
            true
        });
        assert_eq!(seen, ["__init__.py", "__init__.pyi", ".py", ".pyi"]);
    }

    /// A project-name alias `flaskr/*` -> [`src/flaskr/*`, `flaskr/*`] puts
    /// the missing `src/flaskr/db` first. Before the fix discovery probed only
    /// that first base and returned `Missing`; the fix probes the next base and
    /// returns the flat-layout file.
    #[test]
    fn test_discover_module_python_alias_first_base_misses_binds_next_base() {
        let index = Files {
            paths: vec!["flaskr/__init__.py", "flaskr/db.py", "tests/test_db.py"],
            alias: Some(("flaskr", &["src/flaskr", "flaskr"])),
            ..Files::default()
        };
        assert_eq!(
            discover_module(&index, "tests/test_db.py", "flaskr/db", Language::Python),
            Module::Unique("flaskr/db.py")
        );
    }

    /// The src layout still wins when both bases hold the module: the alias's
    /// first expansion is probed first.
    #[test]
    fn test_discover_module_python_alias_src_layout_first_base_wins() {
        let index = Files {
            paths: vec!["src/flaskr/db.py", "flaskr/db.py", "tests/test_db.py"],
            alias: Some(("flaskr", &["src/flaskr", "flaskr"])),
            ..Files::default()
        };
        assert_eq!(
            discover_module(&index, "tests/test_db.py", "flaskr/db", Language::Python),
            Module::Unique("src/flaskr/db.py")
        );
    }

    #[test]
    fn test_python_candidates_stop_when_visit_returns_false() {
        let mut seen = Vec::new();
        python_candidates("a/x", &mut |candidate| {
            seen.push(candidate.to_string());
            seen.len() < 2
        });
        assert_eq!(seen, ["a/x/__init__.py", "a/x/__init__.pyi"]);
    }

    #[test]
    fn test_discover_module_python_relative_climbs_packages_returns_unique() {
        assert_eq!(
            python("pkg/sub/mod.py", "..util"),
            Module::Unique("pkg/util.py")
        );
        assert_eq!(
            python("pkg/sub/mod.py", ".."),
            Module::Unique("pkg/__init__.py")
        );
        assert_eq!(
            python("pkg/sub/mod.py", "."),
            Module::Unique("pkg/sub/__init__.py")
        );
    }

    #[test]
    fn test_discover_module_python_relative_above_root_returns_missing() {
        assert_eq!(python("pkg/sub/mod.py", "...."), Module::Missing);
        assert_eq!(python("pkg/sub/mod.py", ".nope"), Module::Missing);
    }

    #[test]
    fn test_discover_module_python_package_directory_returns_init() {
        assert_eq!(python("main.py", "pkg"), Module::Unique("pkg/__init__.py"));
        assert_eq!(
            python("main.py", "pkg/sub"),
            Module::Unique("pkg/sub/__init__.py")
        );
    }

    #[test]
    fn test_discover_module_python_source_root_returns_unique() {
        assert_eq!(
            python("main.py", "lib/tool"),
            Module::Unique("src/lib/tool.py")
        );
    }

    #[test]
    fn test_discover_module_python_two_source_roots_returns_ambiguous() {
        assert_eq!(python("main.py", "dup/x"), Module::Ambiguous);
    }

    #[test]
    fn test_discover_module_python_package_is_not_a_source_root_returns_missing() {
        // `pkg/util.py` sits under `pkg/`, which is itself a package.
        assert_eq!(python("main.py", "util"), Module::Missing);
    }

    #[test]
    fn test_discover_module_empty_or_blank_specifier_returns_missing() {
        for language in [Language::Python, Language::TypeScript, Language::Php] {
            for specifier in ["", "   "] {
                assert_eq!(
                    discover_module(&files(PYTHON), "main.py", specifier, language),
                    Module::Missing,
                    "{language:?} {specifier:?}"
                );
            }
        }
    }

    #[test]
    fn test_discover_module_python_other_language_file_returns_missing() {
        let index = files(&["pkg/util.ts"]);
        assert_eq!(
            discover_module(&index, "main.py", "pkg/util", Language::Python),
            Module::Missing
        );
    }

    #[test]
    fn test_discover_module_typescript_directory_index_returns_unique() {
        let index = files(&["src/util.ts", "src/lib/index.ts"]);
        let ts = |specifier| discover_module(&index, "src/app.ts", specifier, Language::TypeScript);
        assert_eq!(ts("./util"), Module::Unique("src/util.ts"));
        assert_eq!(ts("./lib"), Module::Unique("src/lib/index.ts"));
        assert_eq!(ts("./nope"), Module::Missing);
    }

    #[test]
    fn test_discover_module_rust_mod_rs_returns_unique() {
        let index = files(&["src/main.rs", "src/util.rs", "src/net/mod.rs"]);
        let rs = |specifier| discover_module(&index, "src/main.rs", specifier, Language::Rust);
        assert_eq!(rs("crate::util"), Module::Unique("src/util.rs"));
        assert_eq!(rs("crate::net"), Module::Unique("src/net/mod.rs"));
        assert_eq!(rs("crate::nope"), Module::Missing);
    }

    fn php() -> Files {
        Files {
            paths: vec![
                "src/Util/Helper.php",
                "src/Util/Other.php",
                "lib/Acme/Tool.php",
                "x/Lib/Thing.php",
                "y/Lib/Thing.php",
                "App/Shadow/Exception.php",
            ],
            namespaces: vec![
                ("src/Util/Helper.php", "App.Util"),
                ("src/Util/Other.php", "App.Util"),
                ("App/Shadow/Exception.php", "Elsewhere"),
            ],
            decls: vec![
                ("src/Util/Helper.php", "Helper"),
                ("src/Util/Other.php", "Other"),
            ],
            scope_root: None,
            alias: None,
        }
    }

    fn php_module(specifier: &str) -> Module<&'static str> {
        discover_module(&php(), "src/main.php", specifier, Language::Php)
    }

    #[test]
    fn test_discover_module_fqn_namespace_keeps_files_declaring_member() {
        let expected = Module::Namespace(Arc::from(["src/Util/Helper.php"]));
        assert_eq!(php_module("App\\Util\\Helper"), expected);
        assert_eq!(php_module("\\App\\Util\\Helper"), expected);
    }

    #[test]
    fn test_discover_module_fqn_namespace_itself_returns_every_declarer() {
        let Module::Namespace(files) = php_module("App\\Util") else {
            panic!("a declared namespace is a Namespace answer");
        };
        let mut files = files.to_vec();
        files.sort_unstable();
        assert_eq!(files, ["src/Util/Helper.php", "src/Util/Other.php"]);
    }

    #[test]
    fn test_discover_module_fqn_namespace_without_member_returns_empty_namespace() {
        assert_eq!(
            php_module("App\\Util\\Nope"),
            Module::Namespace(Arc::from(Vec::<&str>::new()))
        );
    }

    #[test]
    fn test_discover_module_fqn_placed_file_returns_unique() {
        assert_eq!(
            php_module("Acme\\Tool"),
            Module::Unique("lib/Acme/Tool.php")
        );
    }

    #[test]
    fn test_discover_module_fqn_two_placed_files_returns_ambiguous() {
        assert_eq!(php_module("Lib\\Thing"), Module::Ambiguous);
    }

    #[test]
    fn test_discover_module_fqn_namesake_in_other_namespace_returns_missing() {
        assert_eq!(php_module("App\\Shadow\\Exception"), Module::Missing);
    }

    #[test]
    fn test_discover_module_fqn_unknown_prefix_returns_missing() {
        assert_eq!(php_module("Vendor\\Pkg\\Thing"), Module::Missing);
    }

    /// R-A4b: of two source roots, the one under the importer's scope root
    /// holds the module. The repo root, a root that holds neither and no
    /// root leave the import ambiguous.
    #[test]
    fn test_discover_module_python_two_source_roots_scope_root_returns_own() {
        let scoped = |root: Option<&'static str>| Files {
            scope_root: root,
            ..files(PYTHON)
        };
        assert_eq!(
            discover_module(&scoped(Some("a")), "a/main.py", "dup/x", Language::Python),
            Module::Unique("a/dup/x.py")
        );
        assert_eq!(
            discover_module(&scoped(Some("b")), "b/main.py", "dup/x", Language::Python),
            Module::Unique("b/dup/x.py")
        );
        for root in [None, Some(""), Some("c"), Some("a/dup/x.py")] {
            assert_eq!(
                discover_module(&scoped(root), "a/main.py", "dup/x", Language::Python),
                Module::Ambiguous,
                "{root:?}"
            );
        }
        // One root: the scope root decides nothing.
        assert_eq!(
            discover_module(
                &scoped(Some("b")),
                "b/main.py",
                "lib/tool",
                Language::Python
            ),
            Module::Unique("src/lib/tool.py")
        );
    }

    /// R-A4b: two packages declare one namespace and member, and two place
    /// one file: the importer's scope root picks its own; the namespace
    /// itself still names every declarer.
    #[test]
    fn test_discover_module_fqn_two_packages_scope_root_returns_own() {
        let two = |root: Option<&'static str>| Files {
            paths: vec!["x/src/Util.php", "y/src/Util.php"],
            namespaces: vec![("x/src/Util.php", "App"), ("y/src/Util.php", "App")],
            decls: vec![("x/src/Util.php", "Util"), ("y/src/Util.php", "Util")],
            scope_root: root,
            alias: None,
        };
        let import = |files: &Files, specifier: &str| {
            discover_module(files, "y/src/Main.php", specifier, Language::Php)
        };
        assert_eq!(
            import(&two(Some("y")), "App\\Util"),
            Module::Namespace(Arc::from(["y/src/Util.php"]))
        );
        for root in [None, Some(""), Some("z")] {
            let Module::Namespace(both) = import(&two(root), "App\\Util") else {
                panic!("a declared namespace is a Namespace answer");
            };
            assert_eq!(both.len(), 2, "{root:?}");
        }
        let Module::Namespace(itself) = import(&two(Some("y")), "App") else {
            panic!("a declared namespace is a Namespace answer");
        };
        assert_eq!(itself.len(), 2);
        let placed = Files {
            scope_root: Some("y"),
            ..php()
        };
        assert_eq!(
            discover_module(&placed, "y/main.php", "Lib\\Thing", Language::Php),
            Module::Unique("y/Lib/Thing.php")
        );
    }

    #[test]
    fn test_module_map_converts_every_file() {
        let module = Module::Namespace(Arc::from(["a.php", "b.php"]));
        assert_eq!(
            module.map(|file| file.len()),
            Module::Namespace(Arc::from([5, 5]))
        );
        assert_eq!(
            Module::Unique("a.php").map(|file| file.len()),
            Module::Unique(5)
        );
        assert_eq!(
            Module::<&str>::Missing.map(|file| file.len()),
            Module::Missing
        );
    }
}
