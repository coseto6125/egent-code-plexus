//! Class membership of one file's parse: `HasMethod` / `HasProperty` from a
//! type to each member it declares.
//!
//! The index's post-process pass (`ecp-analyzer`
//! `post_process::class_membership`) and the overlay's per-file replay
//! (`session::incremental`) both call [`class_membership`], so a dirty file
//! gets the edges a reindex gives. Each side answers the name lookups of
//! [`MemberLookup`] from its own tables of the file's declarations.
//!
//! Two passes, in this order (spec
//! `docs/superpowers/specs/2026-05-16-class-membership-postprocess.md`):
//!
//! - **span** — the innermost type whose span contains a member owns it:
//!   every language whose type body encloses its members.
//! - **owner** — the parser's `owner_class` names a type whose spans do not
//!   contain the member: a Rust `impl` block, a Go receiver method.
//!
//! One edge kind per target kind (convention B.1): `HasMethod` for a
//! Function, Method or Constructor, `HasProperty` for a Property or a type's
//! Const, so `MATCH (c:Class)-[:HasMethod]->(m)` works in every language. A
//! scoped member (a Rust nested `fn`, a closure node) is no member of any
//! type, even inside a trait's default method: both passes skip it.

use crate::analyzer::types::{span_contains, Span};
use crate::graph::{NodeKind, RelType};
use rustc_hash::FxHashMap;

/// `Edge.reason` of a membership edge of the span pass.
pub const CLASS_MEMBERSHIP_REASON: &str = "post_process:class_membership";

/// `Edge.reason` of a membership edge of the owner pass.
pub const CLASS_MEMBERSHIP_IMPL_REASON: &str = "post_process:class_membership:rust_impl";

/// The fields of one declaration that membership reads. `span` is 0-based,
/// as `RawNode.span`. `arity` is the declared generic arity
/// (`RawNode::generic_arity`); only a type's is read.
#[derive(Debug, Clone, Copy)]
pub struct MemberRow<'a> {
    pub kind: NodeKind,
    pub owner: Option<&'a str>,
    pub name: &'a str,
    pub span: Span,
    pub arity: u32,
}

/// The name lookups membership makes over one path's declarations. Ids are
/// the caller's node ids; `raw` is a row index of the rows passed to
/// [`class_membership`].
pub trait MemberLookup {
    /// The first declaration of the path with this name, kind and owner key:
    /// the index's `SymbolTable::first_declaration`.
    fn first_declaration(&self, name: &str, kind: NodeKind, owner: Option<&str>) -> Option<u32>;
    /// The first declaration of the path with this name, any kind:
    /// `SymbolTable::lookup_in_file`.
    fn lookup_in_file(&self, name: &str) -> Option<u32>;
    /// The later overloads of `first`, in source order:
    /// `SymbolTable::later_overloads`.
    fn later_overloads(&self, first: u32) -> &[u32];
    /// The C# type of `arity` type arguments among `id` and its arity
    /// twins: `SymbolTable::with_arity`.
    fn with_arity(&self, id: u32, arity: u32) -> Option<u32>;
    /// Whether the row `raw` is a scoped node (a lexical parent declares it).
    fn is_scoped(&self, raw: usize) -> bool;
    /// The node of the row `raw` when it is a C# arity twin, which no name
    /// table holds.
    fn twin(&self, raw: usize) -> Option<u32>;
}

/// One membership edge: `source` owns `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership {
    pub source: u32,
    pub target: u32,
    /// `HasMethod` or `HasProperty`.
    pub rel_type: RelType,
    /// [`CLASS_MEMBERSHIP_REASON`] or [`CLASS_MEMBERSHIP_IMPL_REASON`].
    pub reason: &'static str,
}

/// A type that can own members, at row `raw`.
#[derive(Debug, Clone, Copy)]
struct Owner<'a> {
    name: &'a str,
    kind: NodeKind,
    span: Span,
    arity: u32,
    raw: usize,
}

/// Area proxy for picking the smallest enclosing span: rows first, then the
/// column distance. The column term is offset by 2^32 so a span starting past
/// column 10 000 (minified source) still ranks below one that contains it.
#[inline]
pub fn span_area(s: Span) -> u64 {
    let (r1, c1, r2, c2) = s;
    let dr = r2.saturating_sub(r1) as u64;
    let dc = (c2 as u64 + (1 << 32)) - c1 as u64;
    (dr.min(u32::MAX as u64 >> 1) << 33) + dc
}

/// The membership edges of one file's rows, the span pass's first, each in
/// row order. `lookup` runs only for a file that declares a type, so a file
/// without one pays one pass over its rows. Cost: O(rows x types of the
/// file) span checks plus O(rows) lookups.
pub fn class_membership<'a, T, L: MemberLookup>(
    rows: &'a [T],
    row: impl Fn(&'a T) -> MemberRow<'a>,
    lookup: impl FnOnce() -> L,
    mut emit: impl FnMut(Membership),
) {
    // Both passes loop over this per-file index of types (K = types per
    // file, typically 1-10) instead of rescanning the rows. An Enum owns
    // only its constants; an annotation type owns its members like a class,
    // and a use-site annotation (Dart `@override`) spans no member.
    let mut owners: Vec<Owner<'a>> = Vec::new();
    let mut classes_by_name: FxHashMap<&'a str, Vec<Span>> = FxHashMap::default();
    for (raw, r) in rows.iter().map(&row).enumerate() {
        match r.kind {
            NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Interface
            | NodeKind::Annotation => {
                owners.push(Owner {
                    name: r.name,
                    kind: r.kind,
                    span: r.span,
                    arity: r.arity,
                    raw,
                });
                classes_by_name.entry(r.name).or_default().push(r.span);
            }
            NodeKind::Enum => owners.push(Owner {
                name: r.name,
                kind: r.kind,
                span: r.span,
                arity: 0,
                raw,
            }),
            _ => {}
        }
    }
    if owners.is_empty() {
        return;
    }
    let lookup = lookup();
    let later = later_overload_ids(rows, &row, &lookup);
    span_pass(rows, &row, &owners, &lookup, &later, &mut emit);
    owner_pass(rows, &row, &classes_by_name, &lookup, &later, &mut emit);
}

/// Row index → node id of each later overload of the file
/// ([`crate::uid::keeps_overloads`]). A name lookup returns the first
/// declaration, so without this map every overload's edge would land on it.
fn later_overload_ids<'a, T>(
    rows: &'a [T],
    row: &impl Fn(&'a T) -> MemberRow<'a>,
    lookup: &impl MemberLookup,
) -> FxHashMap<usize, u32> {
    let mut seen: FxHashMap<(u8, Option<&'a str>, &'a str), usize> = FxHashMap::default();
    let mut ids = FxHashMap::default();
    for (i, r) in rows.iter().map(row).enumerate() {
        if !crate::uid::keeps_overloads(r.kind) {
            continue;
        }
        let ordinal = seen.entry((r.kind as u8, r.owner, r.name)).or_insert(0);
        if *ordinal > 0 {
            if let Some(&id) = lookup
                .first_declaration(r.name, r.kind, r.owner)
                .and_then(|first| lookup.later_overloads(first).get(*ordinal - 1))
            {
                ids.insert(i, id);
            }
        }
        *ordinal += 1;
    }
    ids
}

/// The node of `member` (row `i`) for an edge from `class`, or `None` to
/// emit none: its own later overload, else the declaration its kind and
/// owner match, else the file's first declaration of the name. Only that
/// last lookup can return the class itself, for a member named like it
/// (Java `class Foo { Foo() {} }`), which would be a self-loop.
fn member_node(
    i: usize,
    member: MemberRow<'_>,
    lookup: &impl MemberLookup,
    later: &FxHashMap<usize, u32>,
    class: u32,
) -> Option<u32> {
    let id = later
        .get(&i)
        .copied()
        .or_else(|| lookup.first_declaration(member.name, member.kind, member.owner))
        .or_else(|| lookup.lookup_in_file(member.name))?;
    (id != class).then_some(id)
}

/// Span pass: the innermost owner around each Function, Method,
/// Constructor, Property or owned Const owns it.
fn span_pass<'a, T>(
    rows: &'a [T],
    row: &impl Fn(&'a T) -> MemberRow<'a>,
    owners: &[Owner<'a>],
    lookup: &impl MemberLookup,
    later: &FxHashMap<usize, u32>,
    emit: &mut impl FnMut(Membership),
) {
    for (i, member) in rows.iter().map(row).enumerate() {
        if lookup.is_scoped(i) {
            continue;
        }
        let rel_type = match member.kind {
            NodeKind::Function | NodeKind::Method | NodeKind::Constructor => RelType::HasMethod,
            NodeKind::Property => RelType::HasProperty,
            // A class constant (PHP `const STATUS`) carries its class as
            // owner, so `scope_defines` gives it no File edge.
            NodeKind::Const if member.owner.is_some() => RelType::HasProperty,
            _ => continue,
        };

        let Some(class) = owners
            .iter()
            .filter(|o| {
                (member.kind == NodeKind::Const || o.kind != NodeKind::Enum)
                    && span_contains(o.span, member.span)
            })
            .min_by_key(|o| span_area(o.span))
            .copied()
        else {
            continue;
        };

        // Namespace-qualified type identities are not represented by the
        // short-name lookup below. Do not bind an ambiguous type constant.
        if member.kind == NodeKind::Const
            && (member.owner != Some(class.name)
                || owners.iter().filter(|o| o.name == class.name).count() != 1)
        {
            continue;
        }

        // The name lookup returns the first of same-named C# types of
        // different arity; the owner is the one of its own arity, and a
        // twin no arity tells apart (`Inner` in `Outer` and in `Outer<T>`)
        // is its own row's node. The class's own owner tells a nested type
        // (`Outer.Employee`) from a same-name type of the file.
        let class_owner = row(&rows[class.raw]).owner;
        let Some(class_idx) = lookup.twin(class.raw).or_else(|| {
            lookup
                .first_declaration(class.name, class.kind, class_owner)
                .or_else(|| lookup.lookup_in_file(class.name))
                .map(|first| lookup.with_arity(first, class.arity).unwrap_or(first))
        }) else {
            continue;
        };
        let Some(member_idx) = lookup
            .twin(i)
            .or_else(|| member_node(i, member, lookup, later, class_idx))
        else {
            continue;
        };
        emit(Membership {
            source: class_idx,
            target: member_idx,
            rel_type,
            reason: CLASS_MEMBERSHIP_REASON,
        });
    }
}

/// Owner pass: a member whose `owner_class` names a type of the file that no
/// span of that name contains (a Rust `impl` block, a Go receiver method).
/// A member some span of its owner's name contains is the span pass's.
fn owner_pass<'a, T>(
    rows: &'a [T],
    row: &impl Fn(&'a T) -> MemberRow<'a>,
    classes_by_name: &FxHashMap<&'a str, Vec<Span>>,
    lookup: &impl MemberLookup,
    later: &FxHashMap<usize, u32>,
    emit: &mut impl FnMut(Membership),
) {
    for (i, member) in rows.iter().map(row).enumerate() {
        if lookup.is_scoped(i) {
            continue;
        }
        let rel_type = match member.kind {
            NodeKind::Function | NodeKind::Method | NodeKind::Constructor => RelType::HasMethod,
            NodeKind::Property => RelType::HasProperty,
            _ => continue,
        };
        let Some(class_name) = member.owner else {
            continue;
        };
        let spanned = classes_by_name
            .get(class_name)
            .is_some_and(|spans| spans.iter().any(|s| span_contains(*s, member.span)));
        if spanned {
            continue;
        }
        let Some(class_idx) = lookup.lookup_in_file(class_name) else {
            continue;
        };
        let Some(member_idx) = member_node(i, member, lookup, later, class_idx) else {
            continue;
        };
        emit(Membership {
            source: class_idx,
            target: member_idx,
            rel_type,
            reason: CLASS_MEMBERSHIP_IMPL_REASON,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One path's declarations, ids = row indexes: the first row of each
    /// (name, kind, owner) stands for the declaration, as the index's
    /// `SymbolTable::first_declaration` does. No overloads, twins or scoped
    /// rows.
    struct Rows<'a>(&'a [MemberRow<'static>]);

    impl MemberLookup for Rows<'_> {
        fn first_declaration(
            &self,
            name: &str,
            kind: NodeKind,
            owner: Option<&str>,
        ) -> Option<u32> {
            self.0
                .iter()
                .position(|r| r.name == name && r.kind == kind && r.owner == owner)
                .map(|i| i as u32)
        }

        fn lookup_in_file(&self, name: &str) -> Option<u32> {
            self.0.iter().position(|r| r.name == name).map(|i| i as u32)
        }

        fn later_overloads(&self, _first: u32) -> &[u32] {
            &[]
        }

        fn with_arity(&self, id: u32, _arity: u32) -> Option<u32> {
            Some(id)
        }

        fn is_scoped(&self, _raw: usize) -> bool {
            false
        }

        fn twin(&self, _raw: usize) -> Option<u32> {
            None
        }
    }

    fn row(
        kind: NodeKind,
        owner: Option<&'static str>,
        name: &'static str,
        span: Span,
    ) -> MemberRow<'static> {
        MemberRow {
            kind,
            owner,
            name,
            span,
            arity: 0,
        }
    }

    fn edges(rows: &[MemberRow<'static>]) -> Vec<Membership> {
        let mut out = Vec::new();
        class_membership(rows, |r| *r, || Rows(rows), |m| out.push(m));
        out
    }

    fn has_method(source: u32, target: u32, reason: &'static str) -> Membership {
        Membership {
            source,
            target,
            rel_type: RelType::HasMethod,
            reason,
        }
    }

    #[test]
    fn test_class_membership_empty_rows_emit_nothing() {
        assert!(edges(&[]).is_empty());
    }

    #[test]
    fn test_class_membership_file_without_type_never_builds_lookup() {
        let rows = [row(NodeKind::Function, None, "free", (0, 0, 1, 0))];
        let mut out = Vec::new();
        class_membership(
            &rows,
            |r| *r,
            || -> Rows<'static> { panic!("a file without a type needs no lookup") },
            |m| out.push(m),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn test_class_membership_nested_class_member_goes_to_innermost() {
        let rows = [
            row(NodeKind::Class, None, "Outer", (1, 0, 20, 0)),
            row(NodeKind::Method, None, "outer_m", (2, 4, 3, 4)),
            row(NodeKind::Class, None, "Inner", (5, 4, 15, 4)),
            row(NodeKind::Method, None, "inner_m", (6, 8, 7, 8)),
        ];
        assert_eq!(
            edges(&rows),
            [
                has_method(0, 1, CLASS_MEMBERSHIP_REASON),
                has_method(2, 3, CLASS_MEMBERSHIP_REASON)
            ]
        );
    }

    #[test]
    fn test_class_membership_member_named_like_class_binds_member_not_self() {
        let rows = [
            row(NodeKind::Class, None, "Foo", (1, 0, 10, 0)),
            row(NodeKind::Method, None, "Foo", (2, 4, 4, 4)),
        ];
        assert_eq!(edges(&rows), [has_method(0, 1, CLASS_MEMBERSHIP_REASON)]);
    }

    #[test]
    fn test_class_membership_owner_outside_span_uses_owner_pass_reason() {
        let rows = [
            row(NodeKind::Struct, None, "Foo", (1, 0, 1, 12)),
            row(NodeKind::Method, Some("Foo"), "fmt", (5, 4, 7, 4)),
        ];
        assert_eq!(
            edges(&rows),
            [has_method(0, 1, CLASS_MEMBERSHIP_IMPL_REASON)]
        );
    }

    #[test]
    fn test_class_membership_owner_inside_span_emits_once() {
        let rows = [
            row(NodeKind::Class, None, "Foo", (1, 0, 10, 0)),
            row(NodeKind::Method, Some("Foo"), "bar", (3, 4, 5, 4)),
        ];
        assert_eq!(edges(&rows), [has_method(0, 1, CLASS_MEMBERSHIP_REASON)]);
    }

    #[test]
    fn test_class_membership_enum_owns_only_its_constant() {
        let rows = [
            row(NodeKind::Enum, None, "Color", (1, 0, 9, 0)),
            row(NodeKind::Const, Some("Color"), "MAX", (2, 4, 2, 20)),
            row(NodeKind::Method, None, "label", (3, 4, 5, 4)),
        ];
        assert_eq!(
            edges(&rows),
            [Membership {
                source: 0,
                target: 1,
                rel_type: RelType::HasProperty,
                reason: CLASS_MEMBERSHIP_REASON,
            }]
        );
    }

    #[test]
    fn test_class_membership_whitespace_only_owner_matches_no_type() {
        let rows = [
            row(NodeKind::Class, None, "K", (1, 0, 2, 0)),
            row(NodeKind::Method, Some(" "), "m", (5, 0, 6, 0)),
        ];
        assert!(edges(&rows).is_empty());
    }
}
