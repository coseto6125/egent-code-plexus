//! JS / TS lexical call scoping: a bare call binds the innermost named
//! function in scope before any name tier. Full indexing (Pass 2) and the
//! session overlay build this same index over one file's declarations.
//!
//! The same lexical facts feed declaration identity: [`file_identities`]
//! numbers one file's declarations for every path that stores a uid.

use crate::analyzer::types::{innermost_enclosing, span_contains, RawNode, Span};
use crate::contracts::identity::{
    assign_file, canonical_path, lexical_parents, DeclIdentity, FileDecl, UidSlot,
};
use crate::file_category::Language;
use crate::graph::NodeKind;
use crate::uid;
use rustc_hash::FxHashMap;
use std::path::Path;

/// The kinds `language` scopes under their enclosing callable (design A
/// §4.2): such a node's uid hashes its parent's anchor instead of an owner.
/// No language opts in yet; slices C to E add them.
pub fn lexical_scope_kinds(_language: Language) -> &'static [NodeKind] {
    &[]
}

/// One declaration as [`file_identities`] reads it: kind, owner, name and
/// 0-based span.
pub type DeclFields<'a> = (NodeKind, Option<&'a str>, &'a str, Span);

/// The [`DeclFields`] of a parsed node.
pub fn raw_decl(node: &RawNode) -> DeclFields<'_> {
    (
        node.kind,
        node.owner_class.as_deref(),
        node.name.as_str(),
        node.span,
    )
}

/// The identity of each declaration of the file at `path`, in slice order:
/// [`assign_file`] over the language's lexical parents. The full build,
/// incremental mode and the overlay view all number a file through this one
/// function, so a later overload or a same-uid redefinition gets the same
/// uid on every path.
pub fn file_identities<'a, T>(
    path: &str,
    decls: &'a [T],
    fields: impl Fn(&'a T) -> DeclFields<'a>,
) -> Vec<DeclIdentity> {
    let path = canonical_path(path);
    let scoped = lexical_scope_kinds(Language::from_path(&path));
    let parents = (!scoped.is_empty()).then(|| {
        let spans: Vec<(NodeKind, Span)> = decls
            .iter()
            .map(|d| {
                let (kind, _, _, span) = fields(d);
                (kind, span)
            })
            .collect();
        lexical_parents(&spans, scoped)
    });
    let decls: Vec<FileDecl<'a>> = decls
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let (kind, owner, name, _) = fields(d);
            FileDecl {
                kind,
                owner,
                name,
                parent: parents.as_ref().and_then(|p| p[i]),
                discriminator: None,
            }
        })
        .collect();
    // `canonical_path` removes every `\`, the one spelling `check_path`
    // rejects, and no parent is set while no language opts in: no
    // `IdentityError` can arise.
    assign_file(&path, &decls).expect("a `/`-separated path and unscoped declarations")
}

/// The uid a node stores for `slot`: a [`UidSlot::Duplicate`] keeps its
/// position as a tombstone ([`uid::tombstone`]), any other slot its own uid.
pub fn stored_uid(slot: UidSlot) -> u64 {
    match slot {
        UidSlot::Duplicate { uid, .. } => uid::tombstone(uid),
        slot => slot.uid(),
    }
}

/// [`stored_uid`] of each declaration of the file at `path`, in slice order.
pub fn file_uids<'a, T>(
    path: &str,
    decls: &'a [T],
    fields: impl Fn(&'a T) -> DeclFields<'a>,
) -> Vec<u64> {
    file_identities(path, decls, fields)
        .into_iter()
        .map(|id| stored_uid(id.slot))
        .collect()
}

/// One file's lexical candidates. Parent discovery runs once, not once per
/// call.
#[derive(Default)]
pub struct LexicalFunctions {
    by_name: FxHashMap<String, Vec<(Option<Span>, u32)>>,
}

impl LexicalFunctions {
    /// The index over `symbols`, one file's declarations in source order as
    /// `(kind, name, has_owner, span)`. The i-th declaration's id is
    /// `start + i`. A file that is not JS / TS gets an empty index.
    pub fn new<'a>(
        path: &Path,
        symbols: impl Iterator<Item = (NodeKind, &'a str, bool, Span)>,
        start: u32,
    ) -> Self {
        let mut index = Self::default();
        if !matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts")
        ) {
            return index;
        }
        let functions: Vec<_> = symbols
            .enumerate()
            .filter(|(_, (kind, ..))| {
                matches!(
                    kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                )
            })
            .collect();
        let spans: Vec<Span> = functions.iter().map(|(_, (.., span))| *span).collect();
        let parents = innermost_enclosing(&spans);
        for (k, &(offset, (kind, name, has_owner, _))) in functions.iter().enumerate() {
            if kind != NodeKind::Function {
                continue;
            }
            // The entry exists even when no candidate joins it: a class's
            // own function named like the callee still keeps the call off
            // the name tiers.
            let candidates = index.by_name.entry(name.to_string()).or_default();
            let parent = parents[k].map(|parent| spans[parent]);
            if parent.is_some() || !has_owner {
                candidates.push((parent, start + offset as u32));
            }
        }
        index
    }

    fn scope_size(scope: Option<Span>) -> (u32, i64) {
        scope
            .map(|span| (span.2 - span.0, span.3 as i64 - span.1 as i64))
            .unwrap_or((u32::MAX, i64::MAX))
    }

    /// The function a bare call to `callee` from `caller` binds, at 1.0.
    /// `None`: the file declares no function of that name, so the name
    /// tiers decide. An empty result prevents fallback to an inaccessible
    /// or ambiguous closure: only the import tiers may bind then.
    pub fn targets(&self, caller: Span, callee: &str) -> Option<Vec<(u32, f32)>> {
        if callee.contains(['.', ':']) {
            return None;
        }
        let candidates = self.by_name.get(callee)?;
        let mut best = None;
        let mut ambiguous = false;
        for &(scope, target) in candidates {
            if scope.is_some_and(|span| !span_contains(span, caller)) {
                continue;
            }
            let size = Self::scope_size(scope);
            match best {
                Some((best_size, _)) if size > best_size => {}
                Some((best_size, _)) if size == best_size => ambiguous = true,
                _ => {
                    best = Some((size, target));
                    ambiguous = false;
                }
            }
        }
        Some(match best {
            Some((_, target)) if !ambiguous => vec![(target, 1.0)],
            _ => Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(path: &str, symbols: &[(NodeKind, &str, bool, Span)]) -> LexicalFunctions {
        LexicalFunctions::new(Path::new(path), symbols.iter().copied(), 10)
    }

    #[test]
    fn test_targets_two_top_level_twins_returns_empty() {
        let lex = index(
            "a.ts",
            &[
                (NodeKind::Function, "helper", false, (0, 0, 0, 27)),
                (NodeKind::Function, "helper", false, (1, 0, 1, 27)),
                (NodeKind::Function, "run", false, (2, 0, 2, 40)),
            ],
        );
        assert_eq!(lex.targets((2, 0, 2, 40), "helper"), Some(Vec::new()));
    }

    #[test]
    fn test_targets_nested_function_shadows_top_level_returns_inner() {
        let lex = index(
            "a.js",
            &[
                (NodeKind::Function, "helper", false, (0, 0, 0, 20)),
                (NodeKind::Function, "outer", false, (1, 0, 5, 1)),
                (NodeKind::Function, "helper", false, (2, 2, 2, 20)),
                (NodeKind::Function, "inner", false, (3, 2, 4, 3)),
            ],
        );
        assert_eq!(lex.targets((3, 2, 4, 3), "helper"), Some(vec![(12, 1.0)]));
        assert_eq!(lex.targets((6, 0, 6, 9), "helper"), Some(vec![(10, 1.0)]));
    }

    #[test]
    fn test_targets_owned_top_level_function_only_returns_empty() {
        let lex = index("a.ts", &[(NodeKind::Function, "make", true, (1, 2, 1, 20))]);
        assert_eq!(lex.targets((5, 0, 5, 9), "make"), Some(Vec::new()));
    }

    #[test]
    fn test_targets_qualified_or_unknown_callee_returns_none() {
        let lex = index("a.ts", &[(NodeKind::Function, "f", false, (0, 0, 0, 9))]);
        assert_eq!(lex.targets((1, 0, 1, 9), "o.f"), None);
        assert_eq!(lex.targets((1, 0, 1, 9), "g"), None);
    }

    fn uids(path: &str, decls: &[(NodeKind, Option<&str>, &str)]) -> Vec<u64> {
        file_uids(path, decls, |d| (d.0, d.1, d.2, (0, 0, 0, 0)))
    }

    #[test]
    fn test_file_uids_overload_and_redefinition_returns_index_numbering() {
        let decls = [
            (NodeKind::Method, Some("A"), "save"),
            (NodeKind::Method, Some("A"), "save"),
            (NodeKind::Namespace, None, "Acme"),
            (NodeKind::Namespace, None, "Acme"),
        ];
        let save = uid::compute(NodeKind::Method, "src/A.cs", Some("A"), "save");
        let acme = uid::compute(NodeKind::Namespace, "src/A.cs", None, "Acme");
        assert_eq!(
            uids("src/A.cs", &decls),
            [save, uid::overload(save, 1), acme, !acme],
            "the second save is overload 1; the second Acme is the bit-inverse tombstone"
        );
    }

    #[test]
    fn test_file_uids_backslash_path_returns_forward_slash_uids() {
        let decls = [(NodeKind::Function, None, "f")];
        assert_eq!(
            uids("src\\a.rs", &decls),
            uids("src/a.rs", &decls),
            "the builder hashes the `/` form of a path"
        );
    }

    #[test]
    fn test_file_uids_dot_backslash_name_matches_stored_path() {
        let decls = [
            (NodeKind::Function, None, "f"),
            (NodeKind::Method, Some("A"), "g"),
        ];
        let stored = [
            uid::compute(NodeKind::Function, "./a.rs", None, "f"),
            uid::compute(NodeKind::Method, "./a.rs", Some("A"), "g"),
        ];
        assert_eq!(
            uids(".\\a.rs", &decls),
            stored,
            "a Unix file named `.\\a.rs` is stored `./a.rs`, and the uid hashes that name"
        );
        assert_eq!(uids("./a.rs", &decls), stored);
        assert_ne!(
            uids("./a.rs", &decls)[0],
            uid::compute(NodeKind::Function, "a.rs", None, "f"),
            "`./a.rs` and `a.rs` are two files"
        );
    }

    #[test]
    fn test_file_uids_doubled_dot_prefix_no_panic() {
        let decls = [(NodeKind::Function, None, "f")];
        let stored = [uid::compute(NodeKind::Function, "././a.rs", None, "f")];
        assert_eq!(uids(".\\.\\a.rs", &decls), stored);
        assert_eq!(uids("././a.rs", &decls), stored);
    }

    #[test]
    fn test_file_uids_empty_file_returns_empty() {
        let decls: [(NodeKind, Option<&str>, &str); 0] = [];
        assert!(uids("src/a.rs", &decls).is_empty());
    }

    #[test]
    fn test_lexical_scope_kinds_before_opt_in_returns_empty() {
        for language in [
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Java,
            Language::Kotlin,
            Language::CSharp,
            Language::Go,
            Language::Rust,
            Language::Php,
            Language::Ruby,
            Language::Swift,
            Language::C,
            Language::Cpp,
            Language::Dart,
        ] {
            assert!(lexical_scope_kinds(language).is_empty(), "{language:?}");
        }
    }

    #[test]
    fn test_new_non_ecmascript_path_returns_empty_index() {
        let lex = index("a.py", &[(NodeKind::Function, "f", false, (0, 0, 0, 9))]);
        assert_eq!(lex.targets((1, 0, 1, 9), "f"), None);
    }
}
