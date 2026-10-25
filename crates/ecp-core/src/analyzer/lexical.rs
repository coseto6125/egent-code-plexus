//! JS / TS lexical call scoping: a bare call binds the innermost named
//! function in scope before any name tier. Full indexing (Pass 2) and the
//! session overlay build this same index over one file's declarations.
//!
//! The same lexical facts feed declaration identity: [`file_identities`]
//! numbers one file's declarations for every path that stores a uid.

use crate::analyzer::heritage::HeritageNode;
use crate::analyzer::same_file_pick::{rust_impl_members, ImplNode};
use crate::analyzer::types::{innermost_enclosing, span_contains, RawNode, Span};
use crate::contracts::identity::{
    assign_file, canonical_path, lexical_parents, DeclIdentity, Discriminator, FileDecl, UidSlot,
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
/// [`assign_file`] over the language's lexical parents and, in a Rust file,
/// each impl member's [`Discriminator::ImplTrait`]. The full build,
/// incremental mode and the overlay view (its dirty parse and its base
/// nodes) all number a file through this one function, so a later overload
/// or a same-uid redefinition gets the same uid, and a declaration the same
/// anchor, on every path.
pub fn file_identities<'a, T>(
    path: &str,
    decls: &'a [T],
    fields: impl Fn(&'a T) -> DeclFields<'a>,
) -> Vec<DeclIdentity> {
    let path = canonical_path(path);
    let language = Language::from_path(&path);
    let rows: Vec<Row<'a>> = decls.iter().map(|d| Row(fields(d))).collect();
    let scoped = lexical_scope_kinds(language);
    let parents = (!scoped.is_empty()).then(|| {
        let spans: Vec<(NodeKind, Span)> = rows.iter().map(|r| (r.0 .0, r.0 .3)).collect();
        lexical_parents(&spans, scoped)
    });
    let traits = (language == Language::Rust).then(|| impl_traits(&rows));
    let decls: Vec<FileDecl<'a>> = rows
        .iter()
        .enumerate()
        .map(|(i, &Row((kind, owner, name, _)))| FileDecl {
            kind,
            owner,
            name,
            parent: parents.as_ref().and_then(|p| p[i]),
            discriminator: traits
                .as_ref()
                .and_then(|t| t[i])
                .map(Discriminator::ImplTrait),
        })
        .collect();
    // `canonical_path` removes every `\`, the one spelling `check_path`
    // rejects; `impl_traits` never yields an empty text; and no parent is
    // set while no language opts in: no `IdentityError` can arise.
    assign_file(&path, &decls).expect("a `/`-separated path and unscoped declarations")
}

/// One [`DeclFields`] row, as [`rust_impl_members`] reads it. A row carries
/// no heritage, so only an `Impl` node's span places a member.
#[derive(Clone, Copy)]
struct Row<'a>(DeclFields<'a>);

impl HeritageNode for Row<'_> {
    fn kind(&self) -> NodeKind {
        self.0 .0
    }

    fn span(&self) -> Span {
        self.0 .3
    }

    fn heritage(&self) -> &[String] {
        &[]
    }
}

impl ImplNode for Row<'_> {
    fn name(&self) -> &str {
        self.0 .2
    }

    fn owner_class(&self) -> Option<&str> {
        self.0 .1
    }
}

/// The trait of the `impl` block that declares each method and constructor
/// of a Rust file: the owner of the innermost `Impl` node whose span holds
/// it, as the parser writes that text (`Display`, `Conv<u8>`). `None` for an
/// inherent `impl` (the parser writes owner `""`), for a member outside
/// every `Impl` node, and for every other kind.
///
/// The span is the only source (C1 Open point 7): the base graph stores no
/// heritage per node, and the dirty parse and the base nodes must derive
/// one answer, or a member pairs with nothing. A member of an `impl` that
/// has no `Impl` node (`impl T for &mut X`) therefore stays `None`.
///
/// A repeated block (`impl T for A<u8>` and `impl T for A<u16>` both write
/// self type `A`, owner `T`) is a uid collision: the base keeps the second as
/// a tombstone with no name and no owner. Its members take no trait on
/// either side, the second block of the dirty parse included, or the two
/// sides never agree on the member's anchor.
fn impl_traits<'a>(rows: &[Row<'a>]) -> Vec<Option<&'a str>> {
    let mut seen = rustc_hash::FxHashSet::default();
    let first_block: Vec<bool> = rows
        .iter()
        .map(|row| {
            row.kind() == NodeKind::Impl
                && !row.name().is_empty()
                && seen.insert((row.name(), row.owner_class()))
        })
        .collect();
    let mut traits = vec![None; rows.len()];
    rust_impl_members(rows, |member, block, _| {
        if first_block[block] {
            traits[member] = rows[block].0 .1.filter(|t| !t.trim().is_empty());
        }
    });
    traits
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
    use crate::contracts::identity::{DeclKey, Uniqueness};

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

    fn identities(path: &str, rows: &[DeclFields<'static>]) -> Vec<DeclIdentity> {
        file_identities(path, rows, |d| *d)
    }

    /// `impl <first> for A { fn fmt }` on lines 1-3, then `impl <second> for
    /// A { fn fmt }` on lines 4-6. A trait `""` is an inherent `impl`.
    fn two_impls(first: &'static str, second: &'static str) -> [DeclFields<'static>; 4] {
        [
            (NodeKind::Impl, Some(first), "A", (1, 0, 3, 1)),
            (NodeKind::Method, Some("A"), "fmt", (2, 4, 2, 40)),
            (NodeKind::Impl, Some(second), "A", (4, 0, 6, 1)),
            (NodeKind::Method, Some("A"), "fmt", (5, 4, 5, 40)),
        ]
    }

    fn fmt_anchor(path: &str, trait_name: Option<&str>) -> u64 {
        let key = DeclKey::unscoped(NodeKind::Method, path, Some("A"), "fmt").unwrap();
        match trait_name {
            Some(t) => key
                .with_discriminator(Discriminator::ImplTrait(t))
                .unwrap()
                .anchor(),
            None => key.anchor(),
        }
    }

    #[test]
    fn test_file_identities_rust_trait_impl_member_anchor_follows_trait() {
        let before = identities("src/a.rs", &two_impls("Display", "Debug"));
        let after = identities("src/a.rs", &two_impls("Debug", "Display"));
        assert_eq!(before[1].anchor, fmt_anchor("src/a.rs", Some("Display")));
        assert_eq!(before[3].anchor, fmt_anchor("src/a.rs", Some("Debug")));
        assert_eq!(
            (before[1].anchor, before[3].anchor),
            (after[3].anchor, after[1].anchor),
            "each `fmt` keeps the anchor of its own trait across the swap"
        );
        assert!(before.iter().all(|d| d.uniqueness == Uniqueness::Unique));
        // C1 Open point 1 (a): the discriminator never enters the stored
        // uid, so the second `fmt` in source order stays overload 1.
        let base = uid::compute(NodeKind::Method, "src/a.rs", Some("A"), "fmt");
        assert_eq!(before[1].slot, after[1].slot);
        assert_eq!(before[1].slot, UidSlot::First(base));
        assert_eq!(after[3].slot.uid(), uid::overload(base, 1));
    }

    #[test]
    fn test_file_identities_rust_inherent_impl_member_anchor_is_uid() {
        let ids = identities("src/a.rs", &two_impls("", "Display"));
        assert_eq!(ids[1].anchor, fmt_anchor("src/a.rs", None));
        assert_eq!(ids[1].anchor, ids[1].slot.uid());
        assert_eq!(ids[3].anchor, fmt_anchor("src/a.rs", Some("Display")));
        assert_eq!(ids[1].uniqueness, Uniqueness::Unique);
    }

    #[test]
    fn test_file_identities_rust_two_inherent_impls_same_member_shared() {
        let ids = identities("src/a.rs", &two_impls("", ""));
        assert_eq!(ids[1].anchor, ids[3].anchor);
        assert_eq!(
            ids[3].uniqueness,
            Uniqueness::Shared {
                position: 1,
                count: 2
            }
        );
    }

    #[test]
    fn test_file_identities_rust_whitespace_trait_text_no_discriminator() {
        let ids = identities("src/a.rs", &two_impls("  ", "Debug"));
        assert_eq!(ids[1].anchor, fmt_anchor("src/a.rs", None));
    }

    #[test]
    fn test_file_identities_rust_member_outside_impl_span_no_discriminator() {
        let rows = [
            (NodeKind::Impl, Some("Display"), "A", (1, 0, 3, 1)),
            (NodeKind::Method, Some("A"), "fmt", (8, 4, 8, 40)),
        ];
        let ids = identities("src/a.rs", &rows);
        assert_eq!(ids[1].anchor, fmt_anchor("src/a.rs", None));
    }

    #[test]
    fn test_file_identities_non_rust_impl_rows_no_discriminator() {
        let ids = identities("A.swift", &two_impls("Display", "Debug"));
        assert_eq!(ids[1].anchor, ids[3].anchor);
        assert_eq!(ids[1].anchor, ids[1].slot.uid());
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
