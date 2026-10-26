//! The `Imports` edges of one file's imports: which import record yields
//! which edge, its target, confidence and reason.
//!
//! The index's post-process pass (`ecp-analyzer` `post_process::imports_edges`)
//! calls [`import_edges`] once per file. Each caller answers the lookups of
//! [`ImportEdgeFacts`] from its own resolver and file set, so a replay that
//! answers them for a dirty file gets the edges a reindex gives.
//!
//! The steps, first hit wins per import (spec
//! `docs/superpowers/specs/2026-05-17-imports-edge-emission.md`):
//!
//! | Step | Lookup | Target | Confidence | Reason |
//! |---|---|---|---|---|
//! | 1 | a TS / JS / Python alias through the import tiers alone; else the bound name through the whole ladder; a callable, then a type | the declaration | the tier's | [`ImportReason::Named`] |
//! | 1, barrel | a step-1 hit through a re-export | the barrel's `File` | 0.9 | [`ImportReason::Module`] |
//! | 2 | the last `.` segment of a qualified name, through the whole ladder | the declaration | the tier's | [`ImportReason::Named`] |
//! | 3a, 3b | the specifier's candidate files, as written, then `./`-prefixed | `File` | 0.9 | [`ImportReason::Module`] |
//! | 3c | basename and path suffix, one file only | `File` | 0.6 | [`ImportReason::Guess`] |
//! | 3d | last segment with the importer's extension, one file only | `File` | 0.5 | [`ImportReason::Guess`] |
//! | 3e | the file stem of a Rust `crate::` / `self::` module | `File` | 0.5 | [`ImportReason::Guess`] |
//! | 3f | a directory named like the specifier, one file of the importer's extension | `File` | 0.4 | [`ImportReason::Guess`] |
//!
//! A later step runs only while the earlier steps of the same import added
//! no edge. An edge to a target the file already imports adds nothing, so
//! the later steps of that import still run. Steps 3c to 3f match a name,
//! not a resolved path, and stay below the 0.7 heuristic ceiling.

use crate::analyzer::reexport::ResolveTarget;
use crate::analyzer::rust_paths::rust_module_path_base;
use crate::analyzer::types::RawImport;
use crate::file_category::Language;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

/// Confidence of a module edge: a resolved specifier (steps 3a, 3b) or the
/// barrel a step-1 hit went through.
pub const MODULE_CONFIDENCE: f32 = 0.9;
/// Confidence of a basename + suffix match (step 3c): the specifier's whole
/// path tail agrees.
const SUFFIX_GUESS_CONFIDENCE: f32 = 0.6;
/// Confidence of a last-segment + importer-extension match (step 3d) and of
/// a Rust module-name match (step 3e): one name agrees.
const NAME_GUESS_CONFIDENCE: f32 = 0.5;
/// Confidence of a namespace / module-directory match (step 3f).
const DIRECTORY_GUESS_CONFIDENCE: f32 = 0.4;

/// Why an `Imports` edge exists: its `Edge.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportReason {
    /// Steps 1 and 2: the declaration the import binds.
    Named,
    /// Steps 3a and 3b, and the barrel of a step-1 hit: a resolved module.
    Module,
    /// Steps 3c to 3f: a file matched by name.
    Guess,
}

impl ImportReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Named => "post_process:imports",
            Self::Module => "post_process:imports:module",
            Self::Guess => "post_process:imports:guess",
        }
    }
}

/// One `Imports` edge from the importer's `File` node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportEdge {
    pub target: u32,
    pub confidence: f32,
    pub reason: ImportReason,
}

/// The lookups [`import_edges`] asks of its caller for one importer.
pub trait ImportEdgeFacts {
    /// The declarations `name` binds in the importer as `target`, each with
    /// its tier's confidence, and the barrel module a re-export hop went
    /// through. `binding_only`: the import tiers alone (the index's
    /// `Resolver::resolve_import_binding`); else the whole ladder without a
    /// call site (`Resolver::resolve_import`).
    fn named(
        &self,
        name: &str,
        target: ResolveTarget,
        binding_only: bool,
    ) -> (Vec<(u32, f32)>, Option<Arc<str>>);

    /// The `File` node of `path`, a `/`-separated path: for a path with two
    /// `File` nodes, the last one (the index's `file_node_idx`).
    fn file_node(&self, path: &str) -> Option<u32>;

    /// Visit the candidate files of `specifier` in the resolver's order (the
    /// index's `Resolver::enumerate_candidates`) until `visit` returns false.
    fn for_each_candidate(&self, specifier: &str, visit: &mut dyn FnMut(&str) -> bool);

    /// The name index of steps 3c to 3f. Only those steps call it, so a
    /// caller can build it on first use.
    fn guesses(&self) -> &GuessIndex<'_>;
}

/// Every file with a `File` node, by basename and by directory component:
/// steps 3c to 3f look a name up in one bucket, not in every path.
#[derive(Debug, Default)]
pub struct GuessIndex<'p> {
    by_basename: FxHashMap<&'p str, Vec<(&'p str, u32)>>,
    by_directory: FxHashMap<&'p str, Vec<(&'p str, u32)>>,
}

impl<'p> GuessIndex<'p> {
    /// `files` holds each `/`-separated path once, with its `File` node.
    pub fn new(files: impl IntoIterator<Item = (&'p str, u32)>) -> Self {
        let mut index = Self::default();
        for (path, node) in files {
            let basename = Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path);
            index
                .by_basename
                .entry(basename)
                .or_default()
                .push((path, node));
            for component in path.split('/') {
                if !component.is_empty() && !component.contains('.') {
                    index
                        .by_directory
                        .entry(component)
                        .or_default()
                        .push((path, node));
                }
            }
        }
        index
    }

    /// The one file that is `candidate` or ends with `/<candidate>`; `None`
    /// for no file or several. Several is no answer: a guess between two
    /// files of one name would cross languages.
    fn suffix_single(&self, candidate: &str) -> Option<u32> {
        let basename = Path::new(candidate)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(candidate);
        let bucket = self.by_basename.get(basename)?;
        let needle = format!("/{candidate}");
        let mut hit = None;
        for &(path, node) in bucket {
            if path == candidate || path.ends_with(&needle) {
                if hit.is_some() {
                    return None;
                }
                hit = Some(node);
            }
        }
        hit
    }

    /// The one file under a `directory` component whose path ends with
    /// `ext_dot`; `None` for no file or several.
    fn directory_single(&self, directory: &str, ext_dot: &str) -> Option<u32> {
        let bucket = self.by_directory.get(directory)?;
        let mut hit = None;
        for &(path, node) in bucket {
            if path.ends_with(ext_dot) {
                if hit.is_some() {
                    return None;
                }
                hit = Some(node);
            }
        }
        hit
    }
}

/// Emit the `Imports` edges of `imports`, the imports of `file` (written in
/// `language`) whose `File` node is `source`, in the index's order. Each
/// target gets one edge per file: the first import that reaches it.
pub fn import_edges(
    file: &Path,
    source: u32,
    language: Language,
    imports: &[RawImport],
    facts: &impl ImportEdgeFacts,
    mut emit: impl FnMut(ImportEdge),
) {
    // A PHP import binds its alias (`use A\B as C`, and every group member
    // `use A\{B}`), not the declared name, which may be a same-file
    // declaration's name.
    let php = language == Language::Php;
    // The import tiers of these languages bind an import by its local name.
    // The general tiers keep the declared name: an alias names no
    // declaration.
    let binds_local = matches!(
        language,
        Language::TypeScript | Language::JavaScript | Language::Python
    );
    let mut seen: FxHashSet<u32> = FxHashSet::default();
    let mut add = |target: u32, confidence: f32, reason: ImportReason| -> bool {
        let fresh = seen.insert(target);
        if fresh {
            emit(ImportEdge {
                target,
                confidence,
                reason,
            });
        }
        fresh
    };
    let extension = file.extension().map(|ext| ext.to_string_lossy());
    let mut dot_prefixed = String::new();
    for import in imports {
        let local_name = match &import.alias {
            Some(alias) if php => alias.as_str(),
            _ => import.imported_name.as_str(),
        };
        // Step 1. An alias the import tiers bind is the import's answer, even
        // when every edge it gives is already in the file.
        let aliased = import.alias.as_deref().filter(|alias| {
            binds_local && import.imported_name != "*" && *alias != import.imported_name
        });
        let mut step = aliased
            .map(|alias| named(facts, alias, true, &mut add))
            .unwrap_or_default();
        if !step.bound {
            step = named(facts, local_name, false, &mut add);
        }
        let mut added = step.added;
        // A hit through a re-export lands outside the module the import
        // names. The importer still depends on that barrel, and the edit
        // hook's `importers()` finds a barrel's consumers only through this
        // edge.
        if let Some(barrel) = step
            .barrel
            .as_deref()
            .and_then(|path| facts.file_node(path))
        {
            if barrel != source && add(barrel, MODULE_CONFIDENCE, ImportReason::Module) {
                added += 1;
            }
        }

        // Step 2: the last segment of a Kotlin / Java / PHP qualified name.
        if added == 0 && import.imported_name.contains('.') {
            if let Some(last) = import.imported_name.rsplit('.').next() {
                if !last.is_empty() && last != import.imported_name {
                    added += named(facts, last, false, &mut add).added;
                }
            }
        }
        if added > 0 {
            continue;
        }

        // Step 3: the module, File → File. C-family `#include "alpha.h"` /
        // `#include <alpha.h>` keep their quotes or angle brackets.
        let cleaned = import
            .source
            .trim_matches(|c: char| matches!(c, '"' | '\'' | '<' | '>'));
        // 3a: as written (TS `./a`, Python `.foo` encode relativity).
        let mut hit = probe(facts, cleaned);
        let mut guess = None;
        // 3b: `./`-prefixed, so the resolver joins the importer's directory
        // (Ruby `require_relative 'alpha'`, Go `import "x/pkg"`, C
        // `#include "alpha.h"`).
        if hit.is_none() && !cleaned.starts_with('.') && !cleaned.is_empty() {
            dot_prefixed.clear();
            dot_prefixed.push_str("./");
            dot_prefixed.push_str(cleaned);
            hit = probe(facts, &dot_prefixed);
        }
        // 3c: a C / C++ header under a search-path directory.
        if hit.is_none() && !cleaned.is_empty() {
            hit = facts.guesses().suffix_single(cleaned);
            guess = hit.map(|_| SUFFIX_GUESS_CONFIDENCE);
        }
        // 3d: Go `import "modulePath/pkg"`, whose `go.mod` module prefix is
        // no file-system path: `pkg.<importer extension>`.
        if hit.is_none() && !cleaned.is_empty() {
            if let (Some(last), Some(ext)) = (cleaned.rsplit('/').next(), &extension) {
                hit = facts.guesses().suffix_single(&format!("{last}.{ext}"));
                guess = hit.map(|_| NAME_GUESS_CONFIDENCE);
            }
        }
        // 3e: a Rust `use` the module tree did not place.
        //
        //   `use crate::a;`         -> source="crate", name="a"      -> `a.rs`
        //   `use crate::a::b::Foo;` -> source="crate::a::b"          -> `b.rs`
        //   `use std::io;`          -> external, no match
        //   `use super::Foo;`       -> skipped (the guess has no anchor)
        if hit.is_none() {
            if let (Some(stem), Some(ext)) = (
                rust_module_file_stem(file, &import.source, &import.imported_name),
                &extension,
            ) {
                hit = facts.guesses().suffix_single(&format!("{stem}.{ext}"));
                guess = hit.map(|_| NAME_GUESS_CONFIDENCE);
            }
        }
        // 3f: C# `using NS;` / Swift `import Module` name a directory that
        // holds the implementation file.
        if hit.is_none() && !cleaned.is_empty() && !cleaned.contains('/') {
            if let Some(ext) = &extension {
                hit = facts
                    .guesses()
                    .directory_single(cleaned, &format!(".{ext}"));
                guess = hit.map(|_| DIRECTORY_GUESS_CONFIDENCE);
            }
        }
        if let Some(target) = hit {
            match guess {
                Some(confidence) => add(target, confidence, ImportReason::Guess),
                None => add(target, MODULE_CONFIDENCE, ImportReason::Module),
            };
        }
    }
}

/// What step 1 or 2 found for one name.
#[derive(Default)]
struct Named {
    /// Whether any declaration bound, an edge already in the file included.
    bound: bool,
    /// The edges added.
    added: usize,
    barrel: Option<Arc<str>>,
}

/// `name` as a callable, then as a type ([`ImportEdgeFacts::named`]): each
/// declaration through `add`, and the first barrel either lookup reports.
fn named(
    facts: &impl ImportEdgeFacts,
    name: &str,
    binding_only: bool,
    add: &mut impl FnMut(u32, f32, ImportReason) -> bool,
) -> Named {
    let mut found = Named::default();
    for target in [ResolveTarget::Callable, ResolveTarget::Type] {
        let (targets, via) = facts.named(name, target, binding_only);
        found.barrel = found.barrel.or(via);
        found.bound |= !targets.is_empty();
        for (id, confidence) in targets {
            if add(id, confidence, ImportReason::Named) {
                found.added += 1;
            }
        }
    }
    found
}

/// The first candidate file of `specifier` that has a `File` node.
fn probe(facts: &impl ImportEdgeFacts, specifier: &str) -> Option<u32> {
    let mut hit = None;
    facts.for_each_candidate(specifier, &mut |candidate| {
        let normalized: Cow<'_, str> = if candidate.contains('\\') {
            Cow::Owned(candidate.replace('\\', "/"))
        } else {
            Cow::Borrowed(candidate)
        };
        hit = facts.file_node(&normalized);
        hit.is_none()
    });
    hit
}

/// The module-file stem a Rust `use` names, for step 3e: the last module
/// segment of a `crate::` / `self::` path, the item name for `use crate::x;`
/// (the parser leaves `crate` / `self` as the source then). `super::` and
/// external crates give `None`: probing them matched unrelated internal files
/// (2092 emitted against 137 valid on `.sample_repo`). The layout rule is
/// [`rust_module_path_base`]; a crate without a `src/` ancestor (a `tests/`
/// file) has no base, and keeps the plain last segment.
fn rust_module_file_stem(caller: &Path, source: &str, imported_name: &str) -> Option<String> {
    let joined;
    let specifier = if matches!(source, "crate" | "self") {
        if imported_name.contains("::") {
            return None;
        }
        joined = format!("{source}::{imported_name}");
        joined.as_str()
    } else if source.starts_with("crate::") || source.starts_with("self::") {
        source
    } else {
        return None;
    };
    let last = rust_module_path_base(caller, specifier)
        .and_then(|base| base.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| specifier.rsplit("::").next().map(str::to_string))?;
    Some(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Answers step 1 for callables from `named`, keyed by (name,
    /// binding_only); types bind nothing. Records each candidate probe.
    #[derive(Default)]
    struct Fake<'p> {
        named: FxHashMap<(String, bool), (Vec<(u32, f32)>, Option<Arc<str>>)>,
        files: FxHashMap<&'p str, u32>,
        candidates: FxHashMap<&'p str, Vec<&'p str>>,
        guesses: GuessIndex<'p>,
        probed: RefCell<Vec<String>>,
    }

    impl<'p> Fake<'p> {
        fn with_files(files: &[(&'p str, u32)]) -> Self {
            Self {
                files: files.iter().copied().collect(),
                guesses: GuessIndex::new(files.iter().copied()),
                ..Self::default()
            }
        }

        fn bind(
            &mut self,
            name: &str,
            binding_only: bool,
            targets: &[(u32, f32)],
            barrel: Option<&str>,
        ) {
            self.named.insert(
                (name.to_owned(), binding_only),
                (targets.to_vec(), barrel.map(Arc::from)),
            );
        }
    }

    impl ImportEdgeFacts for Fake<'_> {
        fn named(
            &self,
            name: &str,
            target: ResolveTarget,
            binding_only: bool,
        ) -> (Vec<(u32, f32)>, Option<Arc<str>>) {
            match target {
                ResolveTarget::Callable => self
                    .named
                    .get(&(name.to_owned(), binding_only))
                    .cloned()
                    .unwrap_or_default(),
                _ => (Vec::new(), None),
            }
        }

        fn file_node(&self, path: &str) -> Option<u32> {
            self.files.get(path).copied()
        }

        fn for_each_candidate(&self, specifier: &str, visit: &mut dyn FnMut(&str) -> bool) {
            self.probed.borrow_mut().push(specifier.to_owned());
            for candidate in self.candidates.get(specifier).into_iter().flatten() {
                if !visit(candidate) {
                    return;
                }
            }
        }

        fn guesses(&self) -> &GuessIndex<'_> {
            &self.guesses
        }
    }

    fn import(source: &str, name: &str, alias: Option<&str>) -> RawImport {
        RawImport {
            source: source.to_owned(),
            imported_name: name.to_owned(),
            alias: alias.map(str::to_owned),
            binding_kind: None,
        }
    }

    fn edges_of(
        file: &str,
        language: Language,
        imports: &[RawImport],
        facts: &Fake<'_>,
    ) -> Vec<ImportEdge> {
        let mut out = Vec::new();
        import_edges(Path::new(file), 0, language, imports, facts, |e| {
            out.push(e)
        });
        out
    }

    fn edge(target: u32, confidence: f32, reason: ImportReason) -> ImportEdge {
        ImportEdge {
            target,
            confidence,
            reason,
        }
    }

    #[test]
    fn test_import_reason_as_str_matches_index_reasons() {
        assert_eq!(ImportReason::Named.as_str(), "post_process:imports");
        assert_eq!(ImportReason::Module.as_str(), "post_process:imports:module");
        assert_eq!(ImportReason::Guess.as_str(), "post_process:imports:guess");
    }

    #[test]
    fn test_import_edges_no_imports_emits_nothing() {
        let facts = Fake::with_files(&[("src/a.ts", 5)]);
        assert!(edges_of("src/main.ts", Language::TypeScript, &[], &facts).is_empty());
        assert!(facts.probed.borrow().is_empty());
    }

    #[test]
    fn test_import_edges_named_hit_keeps_tier_confidence_and_skips_module_steps() {
        let mut facts = Fake::with_files(&[("src/a.ts", 5)]);
        facts.bind("dup", false, &[(7, 0.95)], None);
        let imports = [import("./a", "dup", None)];
        assert_eq!(
            edges_of("src/main.ts", Language::TypeScript, &imports, &facts),
            [edge(7, 0.95, ImportReason::Named)]
        );
        assert!(facts.probed.borrow().is_empty());
    }

    #[test]
    fn test_import_edges_barrel_hit_adds_module_edge_to_barrel_file() {
        let mut facts = Fake::with_files(&[("src/index.ts", 4), ("src/impl.ts", 5)]);
        facts.bind("render", false, &[(9, 0.95)], Some("src/index.ts"));
        let imports = [import("./index", "render", None)];
        assert_eq!(
            edges_of("src/main.ts", Language::TypeScript, &imports, &facts),
            [
                edge(9, 0.95, ImportReason::Named),
                edge(4, MODULE_CONFIDENCE, ImportReason::Module)
            ]
        );
    }

    #[test]
    fn test_import_edges_alias_bound_by_import_tiers_skips_declared_name() {
        let mut facts = Fake::default();
        facts.bind("paint", true, &[(3, 0.95)], None);
        facts.bind("render", false, &[(8, 0.7)], None);
        let imports = [import("./a", "render", Some("paint"))];
        assert_eq!(
            edges_of("src/main.ts", Language::TypeScript, &imports, &facts),
            [edge(3, 0.95, ImportReason::Named)]
        );
    }

    #[test]
    fn test_import_edges_php_alias_is_the_looked_up_name() {
        let mut facts = Fake::default();
        facts.bind("C", false, &[(6, 0.95)], None);
        facts.bind("A\\B", false, &[(8, 0.95)], None);
        let imports = [import("A", "A\\B", Some("C"))];
        assert_eq!(
            edges_of("src/m.php", Language::Php, &imports, &facts),
            [edge(6, 0.95, ImportReason::Named)]
        );
    }

    /// A second import of a target the file already imports adds no edge,
    /// so its module steps still run (the index's per-import count).
    #[test]
    fn test_import_edges_deduped_named_hit_falls_through_to_module_step() {
        let mut facts = Fake::with_files(&[("src/b.ts", 11)]);
        facts.bind("dup", false, &[(7, 0.95)], None);
        facts.candidates.insert("./b", vec!["src/b.ts"]);
        let imports = [import("./a", "dup", None), import("./b", "dup", None)];
        assert_eq!(
            edges_of("src/main.ts", Language::TypeScript, &imports, &facts),
            [
                edge(7, 0.95, ImportReason::Named),
                edge(11, MODULE_CONFIDENCE, ImportReason::Module)
            ]
        );
    }

    #[test]
    fn test_import_edges_fqn_last_segment_retry_binds_short_name() {
        let mut facts = Fake::default();
        facts.bind("Alpha", false, &[(2, 0.95)], None);
        let imports = [import("com.x", "com.x.Alpha", None)];
        assert_eq!(
            edges_of("src/B.java", Language::Java, &imports, &facts),
            [edge(2, 0.95, ImportReason::Named)]
        );
    }

    #[test]
    fn test_import_edges_dot_prefixed_probe_is_module_edge() {
        let mut facts = Fake::with_files(&[("lib/alpha.rb", 3)]);
        facts
            .candidates
            .insert("./alpha", vec!["lib/nope.rb", "lib/alpha.rb"]);
        let imports = [import("alpha", "alpha", None)];
        assert_eq!(
            edges_of("lib/beta.rb", Language::Ruby, &imports, &facts),
            [edge(3, MODULE_CONFIDENCE, ImportReason::Module)]
        );
        assert_eq!(*facts.probed.borrow(), ["alpha", "./alpha"]);
    }

    #[test]
    fn test_import_edges_backslash_candidate_matches_slash_path() {
        let mut facts = Fake::with_files(&[("src/a.ts", 5)]);
        facts.candidates.insert("./a", vec!["src\\a.ts"]);
        let imports = [import("./a", "*", None)];
        assert_eq!(
            edges_of("src/main.ts", Language::TypeScript, &imports, &facts),
            [edge(5, MODULE_CONFIDENCE, ImportReason::Module)]
        );
    }

    #[test]
    fn test_import_edges_suffix_guess_binds_unique_header_only() {
        let facts = Fake::with_files(&[("include/alpha.h", 4)]);
        let imports = [import("\"alpha.h\"", "alpha.h", None)];
        assert_eq!(
            edges_of("src/beta.c", Language::C, &imports, &facts),
            [edge(4, SUFFIX_GUESS_CONFIDENCE, ImportReason::Guess)]
        );
        let twice = Fake::with_files(&[("include/alpha.h", 4), ("vendor/alpha.h", 8)]);
        assert!(edges_of("src/beta.c", Language::C, &imports, &twice).is_empty());
    }

    #[test]
    fn test_import_edges_last_segment_guess_takes_importer_extension() {
        let facts = Fake::with_files(&[("pkg/alpha.go", 6), ("pkg/alpha.py", 9)]);
        let imports = [import("sample/alpha", "alpha", None)];
        assert_eq!(
            edges_of("cmd/main.go", Language::Go, &imports, &facts),
            [edge(6, NAME_GUESS_CONFIDENCE, ImportReason::Guess)]
        );
    }

    #[test]
    fn test_import_edges_directory_guess_needs_one_file_of_importer_extension() {
        let facts = Fake::with_files(&[("src/X/Alpha.cs", 2), ("src/X/notes.md", 3)]);
        let imports = [import("X", "X", None)];
        assert_eq!(
            edges_of("src/Y/Beta.cs", Language::CSharp, &imports, &facts),
            [edge(2, DIRECTORY_GUESS_CONFIDENCE, ImportReason::Guess)]
        );
        let two = Fake::with_files(&[("src/X/Alpha.cs", 2), ("src/X/Gamma.cs", 3)]);
        assert!(edges_of("src/Y/Beta.cs", Language::CSharp, &imports, &two).is_empty());
    }

    #[test]
    fn test_import_edges_missing_module_emits_nothing() {
        let facts = Fake::with_files(&[("src/a.ts", 5)]);
        let imports = [import("./gone", "gone", None), import("", "", None)];
        assert!(edges_of("src/main.ts", Language::TypeScript, &imports, &facts).is_empty());
    }

    #[test]
    fn test_rust_module_file_stem_anchored_paths_only() {
        let caller = Path::new("src/b.rs");
        assert_eq!(
            rust_module_file_stem(caller, "crate", "a").as_deref(),
            Some("a")
        );
        assert_eq!(
            rust_module_file_stem(caller, "crate::a::b", "Foo").as_deref(),
            Some("b")
        );
        assert_eq!(rust_module_file_stem(caller, "std::io", "Read"), None);
        assert_eq!(rust_module_file_stem(caller, "super", "Foo"), None);
        assert_eq!(rust_module_file_stem(caller, "crate", "a::b"), None);
    }
}
