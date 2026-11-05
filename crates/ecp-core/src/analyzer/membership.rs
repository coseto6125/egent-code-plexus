//! Class membership of one file's parse: `HasMethod` / `HasProperty` from a
//! type to each member it declares.
//!
//! The index's post-process pass (`ecp-analyzer`
//! `post_process::class_membership`) and the overlay's per-file replay
//! (`session::incremental`) both call [`class_membership`], so a dirty file
//! gets the edges a reindex gives. Each side answers [`MemberLookup`] from
//! its own map of the file's rows to nodes and its own name table.
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
use crate::contracts::identity::container_parents;
use crate::graph::{NodeKind, RelType};
use rustc_hash::{FxHashMap, FxHashSet};

/// `Edge.reason` of a membership edge of the span pass.
pub const CLASS_MEMBERSHIP_REASON: &str = "post_process:class_membership";

/// `Edge.reason` of a membership edge of the owner pass for a member of a
/// Rust `impl` block.
pub const CLASS_MEMBERSHIP_IMPL_REASON: &str = "post_process:class_membership:rust_impl";

/// `Edge.reason` of a membership edge of the owner pass for a member that
/// names its type by `owner_class` outside the type's span: a Go receiver
/// method, a Swift `extension` member, a C++ out-of-line `Cls::m`.
pub const CLASS_MEMBERSHIP_OWNER_REASON: &str = "post_process:class_membership:owner";

/// The fields of one declaration that membership reads. `span` is 0-based,
/// as `RawNode.span`.
#[derive(Debug, Clone, Copy)]
pub struct MemberRow<'a> {
    pub kind: NodeKind,
    pub owner: Option<&'a str>,
    pub name: &'a str,
    pub span: Span,
}

/// What membership asks of the caller about one file's rows. Ids are the
/// caller's node ids; `raw` is a row index of the rows passed to
/// [`class_membership`].
pub trait MemberLookup {
    /// The node of the row `raw`: its own, or the survivor a merged row
    /// merges into (`RawIndexMap::node`).
    fn node(&self, raw: usize) -> Option<u32>;
    /// Whether some row of the file merges into another row's node, so two
    /// rows can give one edge.
    fn has_merged(&self) -> bool;
    /// Whether the row `raw` is a scoped node (a lexical parent declares it).
    fn is_scoped(&self, raw: usize) -> bool;
}

/// One membership edge: `source` owns `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership {
    pub source: u32,
    pub target: u32,
    /// `HasMethod` or `HasProperty`.
    pub rel_type: RelType,
    /// [`CLASS_MEMBERSHIP_REASON`], [`CLASS_MEMBERSHIP_IMPL_REASON`] or
    /// [`CLASS_MEMBERSHIP_OWNER_REASON`].
    pub reason: &'static str,
}

/// A type that can own members, at row `raw`.
#[derive(Debug, Clone, Copy)]
struct Owner<'a> {
    name: &'a str,
    kind: NodeKind,
    span: Span,
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
/// row order. Each end is the node of its own row ([`MemberLookup::node`]),
/// so a type declared twice owns the members of its own body; only the owner
/// pass names its type. An edge two merged rows both give is emitted once.
/// `lookup` runs only for a file that declares a type or an `Impl` block, so
/// a file without either pays one pass over its rows. Cost: O(rows x types of the file) span
/// checks plus O(rows) lookups.
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
    let mut has_impl = false;
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
                    raw,
                });
                classes_by_name.entry(r.name).or_default().push(r.span);
            }
            NodeKind::Enum => owners.push(Owner {
                name: r.name,
                kind: r.kind,
                span: r.span,
                raw,
            }),
            NodeKind::Impl => has_impl = true,
            _ => {}
        }
    }
    // A Rust file of `impl` blocks alone still owns their members.
    if owners.is_empty() && !has_impl {
        return;
    }
    let lookup = lookup();
    let mut seen: Option<FxHashSet<(u32, u32, u8)>> = lookup.has_merged().then(FxHashSet::default);
    let mut emit = |m: Membership| {
        let key = (m.source, m.target, m.rel_type as u8);
        if seen.as_mut().is_none_or(|seen| seen.insert(key)) {
            emit(m);
        }
    };
    span_pass(rows, &row, &owners, &lookup, &mut emit);
    owner_pass(rows, &row, &classes_by_name, has_impl, &lookup, &mut emit);
}

/// Span pass: the innermost owner around each Function, Method,
/// Constructor, Property or owned Const owns it.
fn span_pass<'a, T>(
    rows: &'a [T],
    row: &impl Fn(&'a T) -> MemberRow<'a>,
    owners: &[Owner<'a>],
    lookup: &impl MemberLookup,
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

        // A type constant binds only to the one type of its owner's name:
        // its owner is a short name, which a namespace-qualified type
        // (two same-named types of the file) does not tell apart.
        if member.kind == NodeKind::Const
            && (member.owner != Some(class.name)
                || owners.iter().filter(|o| o.name == class.name).count() != 1)
        {
            continue;
        }

        let (Some(source), Some(target)) = (lookup.node(class.raw), lookup.node(i)) else {
            continue;
        };
        emit(Membership {
            source,
            target,
            rel_type,
            reason: CLASS_MEMBERSHIP_REASON,
        });
    }
}

/// Owner pass: a member whose `owner_class` names a type of the file that no
/// span of that name contains (a Rust `impl` block, a Go receiver method),
/// or a member of a Rust `Impl` block whose type is in no row of the file.
/// A member some span of its owner's name contains is the span pass's.
///
/// A member of a Rust `Impl` block binds the first type row of its owner's name
/// whose container is the block's container (C1-identity.md "Edge sources bind
/// by row"), so `mod a { struct S; impl S { fn m } }` and the same in `mod b`
/// each own their `m`. With no such row the block itself owns the member: a
/// lone `S` of another module is no proof
/// (`mod c { use super::b::S; impl S {} }` beside `mod a { struct S; }`), and
/// import evidence is C1-identity.md Open point 15, but the block is the
/// member's container in every case, a file of `impl` blocks alone included.
/// Any other member (a Go receiver, an out-of-line member) binds the file's
/// first type declaration of the owner's name: a `namespace`, `mod` or
/// package of that name owns no function, and a name with no type row in the
/// file gets no edge. A member named like its type with no such type would be
/// its own owner, and gets no edge.
fn owner_pass<'a, T>(
    rows: &'a [T],
    row: &impl Fn(&'a T) -> MemberRow<'a>,
    classes_by_name: &FxHashMap<&'a str, Vec<Span>>,
    has_impl: bool,
    lookup: &impl MemberLookup,
    emit: &mut impl FnMut(Membership),
) {
    // The container walk runs once per file, and only for a file with an
    // `Impl` row and an owner-pass member; each block's owner is found once.
    let mut chain: Option<Vec<Option<u32>>> = None;
    let mut block_owner: FxHashMap<(u32, &'a str), Option<u32>> = FxHashMap::default();
    let mut type_rows: Option<FxHashMap<&'a str, usize>> = None;
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
        let containers = has_impl.then(|| {
            &*chain.get_or_insert_with(|| {
                let decls: Vec<(NodeKind, Span)> =
                    rows.iter().map(row).map(|r| (r.kind, r.span)).collect();
                container_parents(&decls)
            })
        });
        let block = containers.and_then(|cs| {
            cs[i]
                .filter(|&c| row(&rows[c as usize]).kind == NodeKind::Impl)
                .map(|block| (block, cs))
        });
        let (source, reason) = match block {
            Some((block, containers)) => {
                let owner = *block_owner.entry((block, class_name)).or_insert_with(|| {
                    let scope = containers[block as usize];
                    let owner = rows
                        .iter()
                        .map(row)
                        .enumerate()
                        .find(|&(t, r)| {
                            r.kind.is_type() && r.name == class_name && containers[t] == scope
                        })
                        .map_or(block as usize, |(t, _)| t);
                    lookup.node(owner)
                });
                (owner, CLASS_MEMBERSHIP_IMPL_REASON)
            }
            None => {
                let type_rows = type_rows.get_or_insert_with(|| {
                    let mut by_name = FxHashMap::default();
                    for (t, r) in rows.iter().map(row).enumerate() {
                        if r.kind.declares_type() {
                            by_name.entry(r.name).or_insert(t);
                        }
                    }
                    by_name
                });
                let owner = type_rows.get(class_name).and_then(|&t| lookup.node(t));
                (owner, CLASS_MEMBERSHIP_OWNER_REASON)
            }
        };
        let (Some(source), Some(target)) = (source, lookup.node(i)) else {
            continue;
        };
        if source != target {
            emit(Membership {
                source,
                target,
                rel_type,
                reason,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One path's declarations, ids = row indexes, except that row `t` of
    /// each `(t, s)` of `merged` is merged into row `s`. The first row of a
    /// name stands for it, as the index's `SymbolTable::lookup_in_file`
    /// does. No scoped rows.
    struct Rows<'a> {
        rows: &'a [MemberRow<'static>],
        merged: &'a [(usize, usize)],
    }

    impl MemberLookup for Rows<'_> {
        fn node(&self, raw: usize) -> Option<u32> {
            let survivor = self
                .merged
                .iter()
                .find(|&&(t, _)| t == raw)
                .map_or(raw, |&(_, s)| s);
            (raw < self.rows.len()).then_some(survivor as u32)
        }

        fn has_merged(&self) -> bool {
            !self.merged.is_empty()
        }

        fn is_scoped(&self, _raw: usize) -> bool {
            false
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
        }
    }

    fn edges(rows: &[MemberRow<'static>]) -> Vec<Membership> {
        merged_edges(rows, &[])
    }

    fn merged_edges(rows: &[MemberRow<'static>], merged: &[(usize, usize)]) -> Vec<Membership> {
        let mut out = Vec::new();
        class_membership(rows, |r| *r, || Rows { rows, merged }, |m| out.push(m));
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
            [has_method(0, 1, CLASS_MEMBERSHIP_OWNER_REASON)]
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

    /// Stands for: membership binds by row (C1-identity.md "Edge sources bind
    /// by row"). A type declared twice owns the members of its own body, never
    /// its first's.
    #[test]
    fn test_class_membership_class_declared_twice_each_owns_its_own_body() {
        let rows = [
            row(NodeKind::Class, None, "C", (1, 0, 3, 0)),
            row(NodeKind::Method, Some("C"), "a", (2, 4, 2, 9)),
            row(NodeKind::Class, None, "C", (5, 0, 7, 0)),
            row(NodeKind::Method, Some("C"), "b", (6, 4, 6, 9)),
        ];
        assert_eq!(
            edges(&rows),
            [
                has_method(0, 1, CLASS_MEMBERSHIP_REASON),
                has_method(2, 3, CLASS_MEMBERSHIP_REASON)
            ]
        );
    }

    /// Two merged rows of one property give one `HasProperty`, from the
    /// type to the survivor.
    #[test]
    fn test_class_membership_merged_property_rows_emit_one_edge() {
        let rows = [
            row(NodeKind::Class, None, "K", (1, 0, 9, 0)),
            row(NodeKind::Property, Some("K"), "x", (2, 4, 2, 9)),
            row(NodeKind::Property, Some("K"), "x", (5, 4, 5, 9)),
        ];
        assert_eq!(
            merged_edges(&rows, &[(2, 1)]),
            [Membership {
                source: 0,
                target: 1,
                rel_type: RelType::HasProperty,
                reason: CLASS_MEMBERSHIP_REASON,
            }]
        );
    }

    /// Two overloads of one method: each binds its own row, with no name
    /// lookup that would return the first.
    #[test]
    fn test_class_membership_overloads_each_bind_own_row() {
        let rows = [
            row(NodeKind::Class, None, "K", (1, 0, 9, 0)),
            row(NodeKind::Method, Some("K"), "save", (2, 4, 3, 4)),
            row(NodeKind::Method, Some("K"), "save", (5, 4, 6, 4)),
        ];
        assert_eq!(
            edges(&rows),
            [
                has_method(0, 1, CLASS_MEMBERSHIP_REASON),
                has_method(0, 2, CLASS_MEMBERSHIP_REASON)
            ]
        );
    }

    /// An owner-pass member binds its own row: two `impl Foo` methods of one
    /// name each get an edge.
    #[test]
    fn test_class_membership_owner_pass_member_binds_own_row() {
        let rows = [
            row(NodeKind::Struct, None, "Foo", (1, 0, 1, 12)),
            row(NodeKind::Method, Some("Foo"), "m", (5, 4, 7, 4)),
            row(NodeKind::Method, Some("Foo"), "m", (9, 4, 11, 4)),
        ];
        assert_eq!(
            edges(&rows),
            [
                has_method(0, 1, CLASS_MEMBERSHIP_OWNER_REASON),
                has_method(0, 2, CLASS_MEMBERSHIP_OWNER_REASON)
            ]
        );
    }

    /// Stands for: C1-identity.md "Edge sources bind by row", owner pass,
    /// step 1. `mod a { struct S; impl S { fn m } }` and the same in `mod b`:
    /// each `m` binds the `S` whose container is its `impl` block's container, never
    /// the file's first `S`.
    #[test]
    fn test_class_membership_rust_impl_member_binds_type_beside_its_block() {
        let rows = [
            row(NodeKind::Module, None, "a", (0, 0, 5, 1)),
            row(NodeKind::Struct, Some("a"), "S", (1, 4, 1, 17)),
            row(NodeKind::Impl, None, "S", (2, 4, 4, 5)),
            row(NodeKind::Method, Some("S"), "m", (3, 8, 3, 20)),
            row(NodeKind::Module, None, "b", (6, 0, 11, 1)),
            row(NodeKind::Struct, Some("b"), "S", (7, 4, 7, 17)),
            row(NodeKind::Impl, None, "S", (8, 4, 10, 5)),
            row(NodeKind::Method, Some("S"), "m", (9, 8, 9, 20)),
        ];
        assert_eq!(
            edges(&rows),
            [
                has_method(1, 3, CLASS_MEMBERSHIP_IMPL_REASON),
                has_method(5, 7, CLASS_MEMBERSHIP_IMPL_REASON)
            ]
        );
    }

    /// Stands for: C1-identity.md "Edge sources bind by row", owner pass,
    /// step 2. The only `S` of the file sits in `mod a`, the `impl S` in `mod c`
    /// (which imports another module's `S`): no type of that name beside the
    /// block, so no "only one in the file" fallback to `a::S`; the block owns
    /// `m`.
    #[test]
    fn test_class_membership_rust_impl_member_no_type_beside_block_binds_block() {
        let rows = [
            row(NodeKind::Module, None, "a", (0, 0, 2, 1)),
            row(NodeKind::Struct, Some("a"), "S", (1, 4, 1, 17)),
            row(NodeKind::Module, None, "c", (3, 0, 7, 1)),
            row(NodeKind::Impl, None, "S", (4, 4, 6, 5)),
            row(NodeKind::Method, Some("S"), "m", (5, 8, 5, 20)),
        ];
        assert_eq!(
            edges(&rows),
            [has_method(3, 4, CLASS_MEMBERSHIP_IMPL_REASON)]
        );
    }

    /// Stands for: a file of `impl` blocks alone (no type row) still runs
    /// the owner pass, so the block owns its member.
    #[test]
    fn test_class_membership_rust_impl_only_file_binds_block() {
        let rows = [
            row(NodeKind::Impl, None, "S", (0, 0, 2, 1)),
            row(NodeKind::Method, Some("S"), "m", (1, 4, 1, 20)),
        ];
        assert_eq!(
            edges(&rows),
            [has_method(0, 1, CLASS_MEMBERSHIP_IMPL_REASON)]
        );
    }

    /// An owner-pass member named like its owner, in a file with no type of
    /// that name before it, would own itself: no edge.
    #[test]
    fn test_class_membership_owner_pass_self_owner_emits_nothing() {
        let rows = [
            row(NodeKind::Class, None, "K", (1, 0, 2, 0)),
            row(NodeKind::Method, Some("Foo"), "Foo", (5, 0, 6, 0)),
        ];
        assert!(edges(&rows).is_empty());
    }

    /// Stands for: a module, namespace or package is no type, so it owns no
    /// function through `HasMethod` (`defines` / `contains` carry that).
    #[test]
    fn test_class_membership_module_owner_emits_nothing() {
        for kind in [NodeKind::Module, NodeKind::Namespace] {
            let rows = [
                row(kind, None, "parse", (0, 0, 4, 1)),
                row(NodeKind::Function, Some("parse"), "cb", (1, 4, 2, 5)),
            ];
            assert!(edges(&rows).is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn test_class_membership_module_sharing_a_type_name_binds_the_type() {
        let rows = [
            row(NodeKind::Namespace, None, "Foo", (0, 0, 4, 1)),
            row(NodeKind::Struct, None, "Foo", (6, 0, 6, 12)),
            row(NodeKind::Method, Some("Foo"), "m", (9, 0, 10, 1)),
        ];
        assert_eq!(
            edges(&rows),
            [has_method(1, 2, CLASS_MEMBERSHIP_OWNER_REASON)]
        );
    }

    #[test]
    fn test_class_membership_owner_absent_from_file_emits_nothing() {
        let rows = [row(NodeKind::Method, Some("Elsewhere"), "m", (1, 0, 2, 0))];
        assert!(edges(&rows).is_empty());
    }
}
