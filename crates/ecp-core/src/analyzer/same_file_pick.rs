//! The rule that picks one of a file's same-named declarations for a call
//! site, given the type the call goes through. Index-time resolution
//! (`SymbolTable`) and the session overlay read the same rule from here,
//! each through its own [`PickFacts`], so the two cannot drift.

use crate::analyzer::heritage::HeritageNode;
use crate::analyzer::reexport::ResolveTarget;
use crate::analyzer::types::{owner_key, RawNode};
use crate::file_category::Language;
use crate::graph::NodeKind;
use std::borrow::Cow;

/// How a call site reaches the declarations [`prefer_owned`] weighs.
#[derive(Debug, Clone, Copy, Default)]
pub struct PickScope {
    /// The calling declaration.
    pub caller: Option<u32>,
    /// The owner is the receiver's type (`K.m()`, `this->m()`, a base the
    /// caller inherits), not the caller's type through an implicit `this`.
    pub typed_receiver: bool,
    /// `o.m()` on a receiver the parser could not type
    /// ([`CallSite::UntypedMember`](crate::analyzer::types::CallSite)).
    pub untyped_receiver: bool,
    /// The owner is a type the index declares (`self.m()` typed `C.m`, a
    /// class receiver `K.m()`), not a module or namespace: a call through
    /// it reaches only that type's members and its supertypes'.
    pub qualifier_is_type: bool,
}

/// The owner the pick rules see for one declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickOwner<'a> {
    /// A free function, or a member of a namespace or module.
    Free,
    /// A member of the type with this owner key.
    Type(&'a str),
    /// A member of the Rust `impl` block with this id whose self type the
    /// parser could not name (`impl Trait for &mut Cursor<T>`).
    Block(u32),
}

/// A Rust callable declared in an `impl` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImplMember<'a> {
    /// Identifies the block: no two members of one block share a name.
    pub block: u32,
    /// The owner key of the trait the block implements; `None` for an
    /// inherent `impl`.
    pub trait_owner: Option<&'a str>,
}

/// What the pick rules read about the declarations. Ids are `u32` node
/// handles in the caller's id space; owners compare by [`owner_key`].
pub trait PickFacts {
    /// `file`'s declarations of `name` as a name lookup sees them (the
    /// first of each uid, no later overload), in source order, any kind.
    /// Each is followed by its later overloads that a Rust `impl` block
    /// declares: members of several ownerless blocks share one uid, so all
    /// but the first are overloads.
    fn pick_candidates(&self, file: &str, name: &str) -> Cow<'_, [u32]>;
    fn kind(&self, id: u32) -> NodeKind;
    /// The [`owner_key`] of the type declaring `id`; `None` at top level.
    fn owner(&self, id: u32) -> Option<&str>;
    /// The parser's owner text before [`owner_key`] normalizes it.
    ///
    /// The default keeps existing facts compact.  Rust nested functions use
    /// this only to distinguish lexical owners such as `A::new` and `B::new`.
    fn raw_owner(&self, id: u32) -> Option<&str> {
        self.owner(id)
    }
    /// The `impl` block that declares the Rust callable `id`.
    fn impl_member(&self, id: u32) -> Option<ImplMember<'_>>;
    /// Whether the owner key `owner` names a namespace or module that
    /// `file` declares, and no type of `file`: C++ `detail::to_json`, a C#
    /// namespace's classes, a Rust inline `mod m`'s functions.
    fn is_namespace_owner(&self, file: &str, owner: &str) -> bool;
    /// Whether a namespace declared in `target` is visible to `reader`.
    ///
    /// Targets without a namespace remain visible. Implementations that do
    /// not retain namespace facts keep the preceding resolution behavior.
    fn namespace_visible_from(&self, _reader: &str, _target: &str) -> bool {
        true
    }
    /// The file `id` is declared in.
    fn file_of(&self, id: u32) -> Option<&str>;
    /// Every type declaration named `owner` across the project.
    fn types_named(&self, owner: &str) -> Vec<u32>;
    /// The transitive resolved supertypes of the type `ty`, breadth-first,
    /// each type once, `ty` excluded.
    fn ancestors(&self, ty: u32) -> Vec<u32>;
}

/// The lexical scope of a Rust callable used for same-file free-function
/// lookup. Trait implementations retain their public self-type owner, while
/// nested declarations also need the trait name to stay distinct.
pub fn rust_lexical_owner<F: PickFacts + ?Sized>(facts: &F, id: u32, name: &str) -> String {
    let owner = facts.raw_owner(id);
    match facts.impl_member(id).and_then(|member| member.trait_owner) {
        Some(trait_owner) => owner.map_or_else(
            || format!("{trait_owner}::{name}"),
            |owner| format!("{trait_owner}::{owner}::{name}"),
        ),
        None => owner.map_or_else(|| name.to_string(), |owner| format!("{owner}::{name}")),
    }
}

/// Whether a call through a type receiver in `language` reaches only that
/// type's members and its supertypes' ([`PickScope::qualifier_is_type`]).
/// A Kotlin extension function (free), a C# extension method or a Dart
/// `extension on` member (another type's), a Rust blanket `impl<X> T for X`,
/// and the UFCS or `using for` of other languages reach past that lineage,
/// so those languages keep the by-name rules.
pub fn type_receiver_is_closed(language: Language) -> bool {
    matches!(
        language,
        Language::Python
            | Language::Java
            | Language::JavaScript
            | Language::TypeScript
            | Language::Php
            | Language::Ruby
            | Language::Go
            | Language::Swift
            | Language::C
            | Language::Cpp
    )
}

/// Whether `id` has no owner and no `impl` block either.
pub fn is_free<F: PickFacts + ?Sized>(facts: &F, id: u32) -> bool {
    facts.owner(id).is_none() && facts.impl_member(id).is_none()
}

/// Whether a Rust bare call may reach `id` outside its lexical function.
///
/// An unowned declaration is globally free.  A declaration directly owned by
/// a module remains callable as a module-level free function.  A raw owner
/// that differs from its normalized owner is normally a lexical function path
/// and must not reappear through the global tiers. A path made entirely of
/// declared modules remains a module-level free function.
pub fn rust_free_global_candidate<F: PickFacts + ?Sized>(facts: &F, file: &str, id: u32) -> bool {
    match facts.raw_owner(id) {
        None => true,
        Some(raw_owner) => {
            facts.owner(id) == Some(raw_owner) && facts.is_namespace_owner(file, raw_owner)
                || raw_owner
                    .split("::")
                    .all(|segment| facts.is_namespace_owner(file, segment))
        }
    }
}

/// A receiverless name that cannot reach a method prefers its enclosing
/// lexical function or module's declaration, then an unowned declaration.
pub fn prefer_free<F: PickFacts + ?Sized>(
    facts: &F,
    file: &str,
    member: &str,
    owner: Option<&str>,
    target: ResolveTarget,
) -> Option<u32> {
    let module = owner.and_then(owner_key).and_then(|owner| {
        if facts.is_namespace_owner(file, owner) {
            return Some(owner);
        }
        let declarations = facts.pick_candidates(file, owner);
        let mut types = declarations
            .iter()
            .copied()
            .filter(|&id| facts.kind(id).is_type());
        let Some(ty) = types.next() else {
            let mut functions = declarations
                .iter()
                .copied()
                .filter(|&id| facts.kind(id) == NodeKind::Function);
            return (functions.next().is_some() && functions.next().is_none()).then_some(owner);
        };
        if types.next().is_some() {
            return None;
        }
        facts
            .owner(ty)
            .filter(|owner| facts.is_namespace_owner(file, owner))
    });
    let ids = facts.pick_candidates(file, member);
    let predicate = target.kind_predicate();
    let candidates = || {
        ids.iter().copied().filter(|&id| {
            let kind = facts.kind(id);
            kind != NodeKind::Method && predicate(kind)
        })
    };
    module
        .and_then(|module| candidates().find(|&id| facts.owner(id) == Some(module)))
        .or_else(|| candidates().find(|&id| is_free(facts, id)))
}

/// Like [`prefer_free`], but first chooses one declaration in a known lexical
/// function scope.  The scope uses the parser's raw owner text because
/// [`owner_key`] intentionally collapses `A::new` and `B::new` to `new` for
/// type-member resolution.  An ambiguous lexical scope remains unresolved.
/// It checks each enclosing lexical scope before an unowned free function.
/// Falling back to normalized owners would let `B::new` bind
/// `A::new::helper` merely because both normalize to `new`.
pub fn prefer_free_scoped<F: PickFacts + ?Sized>(
    facts: &F,
    file: &str,
    member: &str,
    lexical_owner: Option<&str>,
    owner: Option<&str>,
    target: ResolveTarget,
) -> Option<u32> {
    if let Some(lexical_owner) = lexical_owner {
        let predicate = target.kind_predicate();
        let ids = facts.pick_candidates(file, member);
        let candidates = || {
            ids.iter()
                .copied()
                .filter(|&id| facts.kind(id) != NodeKind::Method && predicate(facts.kind(id)))
        };
        let mut scope = Some(lexical_owner);
        while let Some(owner) = scope {
            let mut scoped = candidates().filter(|&id| facts.raw_owner(id) == Some(owner));
            match (scoped.next(), scoped.next()) {
                (Some(id), None) => return Some(id),
                (Some(_), Some(_)) => return None,
                (None, _) => {}
            }
            scope = owner.rsplit_once("::").map(|(parent, _)| parent);
        }
        if let Some(id) = prefer_free(facts, file, member, owner, target)
            .filter(|&id| is_free(facts, id) || rust_free_global_candidate(facts, file, id))
        {
            return Some(id);
        }
        return candidates().find(|&id| is_free(facts, id));
    }
    prefer_free(facts, file, member, owner, target)
}

/// The owner the pick rules see for `id`, declared in `file`.
pub fn pick_owner<'f, F: PickFacts + ?Sized>(facts: &'f F, id: u32, file: &str) -> PickOwner<'f> {
    let owner = facts.owner(id);
    if let Some(owner) = owner.filter(|&owner| !facts.is_namespace_owner(file, owner)) {
        return PickOwner::Type(owner);
    }
    match facts.impl_member(id) {
        Some(member) if owner.is_none() => PickOwner::Block(member.block),
        _ => PickOwner::Free,
    }
}

/// The declaration a call of `member` reaches among `file`'s same-named
/// declarations of `target`'s kinds, given `pick`, the first of them in
/// source order. `owner` is the type the call goes through (the caller's
/// type for an implicit-`this` `m()`, `K` for `K.m()`), `heritage` that
/// type's bases in declared order, `scope` how the call site reaches them.
/// The rule:
///
/// 1. For a Rust method call (`ResolveTarget::Method`) from a member of an
///    `impl` block the caller's type or no type owns, that block's own
///    member, the caller excepted: a call of its own name goes through
///    another receiver (`Pin::new(&mut **self).m()`).
/// 2. Else `pick` stays when it is `owner`'s own member, or a free function
///    reached without a receiver.
/// 3. Else `owner`'s own member: Rust inherent methods beat trait methods,
///    and an override hides the declaration it implements.
/// 4. Else the member of the first base in `heritage` that has one.
/// 5. Else `pick` when it is free: a receiver-typed `K.m()` whose qualifier
///    owns no `m` names a module (`mod.f()`).
/// 6. Else the first free function: a member of an unrelated type never
///    beats it. A member of a namespace or module counts as free
///    ([`PickFacts::is_namespace_owner`]); a member of a Rust `impl` block
///    the parser gave no owner does not.
/// 7. Else `pick` when every candidate has one owner (the same type
///    declared in several blocks counts once).
/// 8. Else the member of the owner that every other candidate's owner
///    inherits from or implements ([`hierarchy_root`]).
/// 9. Else `None`: two or more unrelated owners, and the file alone cannot
///    tell which one the call reaches.
///
/// A receiver of a declared type (`scope.qualifier_is_type`) reaches no
/// free function and no member of an unrelated type. Rules 5 to 8 give way
/// to `pick`, else the first candidate, when it is a method the parser gave
/// no owner (its type may be `owner`, as before); else to the member of
/// the supertype of `owner` that overrides every other supertype's one
/// ([`ancestor_member`]); else `None`, which leaves the call to the
/// receiver-typing ladder.
///
/// An untyped receiver (`scope.untyped_receiver`) is no `this`: `owner`,
/// the caller's type, and `heritage` do not narrow the pick, and it never
/// reaches a free function. Rules 3 to 8 weigh the members of every type,
/// the first of them standing in for `pick`; a result that `owner`
/// declares is `None`, since the name alone cannot tell the receiver from
/// the caller's own type. Rule 1 still holds: there the parser could not
/// type `self` either.
///
/// Owners and bases compare through [`owner_key`], so `pkg.B` and `B<T>`
/// name the owner `B`.
#[allow(clippy::too_many_arguments)]
pub fn prefer_owned<F: PickFacts + ?Sized>(
    facts: &F,
    file: &str,
    pick: u32,
    member: &str,
    owner: Option<&str>,
    heritage: &[String],
    target: ResolveTarget,
    scope: PickScope,
) -> Option<u32> {
    let caller = scope.caller;
    let untyped = scope.untyped_receiver;
    let owner = owner.and_then(owner_key);
    // `self.m()` from an `impl` block reaches that block's `m` first: two
    // blocks for one type (`impl W for Cursor<A>`, `.. Cursor<B>`) share
    // the owner, so the owner alone cannot pick the block.
    let caller_block = caller
        .filter(|&caller| {
            let caller_owner = facts.owner(caller);
            target == ResolveTarget::Method && (caller_owner.is_none() || owner == caller_owner)
        })
        .and_then(|caller| facts.impl_member(caller))
        .map(|member| member.block);
    let pick_stays = !untyped
        && ((owner.is_some() && owner == facts.owner(pick))
            || (is_free(facts, pick) && !scope.typed_receiver));
    if pick_stays && caller_block.is_none() {
        return Some(pick);
    }
    let ids = facts.pick_candidates(file, member);
    let predicate = target.kind_predicate();
    let owner_of = |id: u32| pick_owner(facts, id, file);
    let admitted = |id: u32| !untyped || owner_of(id) != PickOwner::Free;
    let all = || ids.iter().copied().filter(|&id| predicate(facts.kind(id)));
    let own_block = caller_block.and_then(|block| {
        all().find(|&id| {
            Some(id) != caller && facts.impl_member(id).is_some_and(|m| m.block == block)
        })
    });
    if own_block.is_some() {
        return own_block;
    }
    if pick_stays {
        return Some(pick);
    }
    let candidates = || all().filter(|&id| admitted(id));
    let pick = if admitted(pick) {
        pick
    } else {
        candidates().next()?
    };
    let owned_by = |wanted: &str| candidates().find(|&id| facts.owner(id) == Some(wanted));
    // A base's own member stands only when no base declared before it
    // reaches the member through its ancestors: `D(B, C)` with `B(A)`
    // resolves to `A.m`, not `C.m`, in Python's method resolution order.
    let inherits_member = |base: &str| {
        let above: Vec<u32> = facts
            .types_named(base)
            .into_iter()
            .flat_map(|ty| facts.ancestors(ty))
            .collect();
        candidates().any(|id| {
            facts
                .owner(id)
                .is_some_and(|o| facts.types_named(o).iter().any(|t| above.contains(t)))
        })
    };
    let bases: Vec<&str> = heritage.iter().filter_map(|base| owner_key(base)).collect();
    // `Some(None)`: a base owns the member but an earlier base may shadow it.
    let lineage = owner
        .filter(|_| !untyped)
        .and_then(owned_by)
        .map(Some)
        .or_else(|| {
            bases.iter().enumerate().find_map(|(at, &base)| {
                owned_by(base).map(|id| {
                    (!bases[..at].iter().any(|&earlier| inherits_member(earlier))).then_some(id)
                })
            })
        });
    let found = if let Some(found) = lineage {
        found
    } else {
        match owner.filter(|_| scope.qualifier_is_type) {
            Some(owner) => {
                let ownerless = |id: u32| facts.kind(id) == NodeKind::Method && is_free(facts, id);
                Some(pick)
                    .filter(|&id| ownerless(id))
                    .or_else(|| candidates().find(|&id| ownerless(id)))
                    .or_else(|| ancestor_member(facts, file, owner, candidates()))
            }
            None => is_free(facts, pick)
                .then_some(pick)
                .or_else(|| candidates().find(|&id| owner_of(id) == PickOwner::Free))
                .or_else(|| {
                    let picked = owner_of(pick);
                    candidates()
                        .all(|id| owner_of(id) == picked)
                        .then_some(pick)
                })
                .or_else(|| hierarchy_root(facts, &candidates().collect::<Vec<_>>(), owner_of)),
        }
    };
    found.filter(|&id| !untyped || owner.is_none() || owner != facts.owner(id))
}

/// The one of `candidates` that a supertype of the type `owner` declares,
/// when that supertype inherits from every other supertype declaring one
/// of them; `None` when two declaring supertypes sit on separate branches
/// (`D(B, C)` with `B(A)`, `A.m` and `C.m`). `owner` is the type `file`
/// declares under that key, else the one type of that key across the
/// project; `None` when neither names exactly the type, since the bases of
/// a namesake would stand in for the receiver's.
pub fn ancestor_member<F: PickFacts + ?Sized>(
    facts: &F,
    file: &str,
    owner: &str,
    candidates: impl Iterator<Item = u32>,
) -> Option<u32> {
    let named = facts.types_named(owner);
    let local: Vec<u32> = named
        .iter()
        .copied()
        .filter(|&ty| facts.file_of(ty) == Some(file))
        .collect();
    let types = match (local.is_empty(), named.len()) {
        (false, _) => local,
        (true, 1) => named,
        _ => return None,
    };
    let owned: Vec<(u32, Vec<u32>)> = candidates
        .filter_map(|id| Some((id, facts.types_named(facts.owner(id)?))))
        .collect();
    let mut declaring: Vec<(u32, u32)> = Vec::new();
    for ancestor in types.iter().flat_map(|&ty| facts.ancestors(ty)) {
        if declaring.iter().any(|&(seen, _)| seen == ancestor) {
            continue;
        }
        if let Some(&(id, _)) = owned.iter().find(|(_, types)| types.contains(&ancestor)) {
            declaring.push((ancestor, id));
        }
    }
    // Breadth-first order is not the method resolution order: `D(B, C)`
    // with `B(A)` reaches `A.m` before `C.m` at runtime. Only the declaring
    // ancestor that inherits every other declaring one is the member the
    // call reaches in every language; with none, the type ladder decides.
    declaring
        .iter()
        .find(|&&(ancestor, _)| {
            let above = facts.ancestors(ancestor);
            declaring
                .iter()
                .all(|&(other, _)| other == ancestor || above.contains(&other))
        })
        .map(|&(_, id)| id)
}

/// Whether an untyped member call (`o.m()`, [`PickScope::untyped_receiver`])
/// may bind `id`: a method or a type's function, never a free function, a
/// namespace or module member, a constructor, or a member of
/// `caller_owner`, the caller's own type, by the name alone.
pub fn member_call_admits<F: PickFacts + ?Sized>(
    facts: &F,
    id: u32,
    caller_owner: Option<&str>,
) -> bool {
    if !matches!(facts.kind(id), NodeKind::Function | NodeKind::Method) {
        return false;
    }
    let file = facts.file_of(id).unwrap_or_default();
    let own = caller_owner.and_then(owner_key);
    pick_owner(facts, id, file) != PickOwner::Free && (own.is_none() || own != facts.owner(id))
}

/// The first of `candidates` whose owner is a type every other candidate's
/// owner inherits from or implements: one declaration and its overrides or
/// implementations (`trait T { fn m(&self); }` beside `impl T for S`), so a
/// call through any of them reaches that declaration. A candidate inherits
/// through the declared supertypes of its owner, found by the owner's key
/// across the project as owned lookups do; a Rust `impl` member implements
/// the trait of its block whatever the block's self type (`impl T for
/// Vec<u8>`, `impl<X> T for X`, a struct of another crate). `owner_of` is
/// [`pick_owner`] for the candidates' file. `None` when no candidate is
/// such a root, or when a candidate is free.
pub fn hierarchy_root<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    candidates: &[u32],
    owner_of: impl Fn(u32) -> PickOwner<'f>,
) -> Option<u32> {
    let types_of = |owner: PickOwner<'_>| match owner {
        PickOwner::Type(owner) => facts.types_named(owner),
        PickOwner::Free | PickOwner::Block(_) => Vec::new(),
    };
    let owners: Vec<PickOwner<'f>> = candidates.iter().map(|&id| owner_of(id)).collect();
    let lineages: Vec<Vec<u32>> = owners
        .iter()
        .map(|&owner| {
            types_of(owner)
                .into_iter()
                .flat_map(|ty| facts.ancestors(ty))
                .collect()
        })
        .collect();
    let root = (0..candidates.len()).find(|&i| {
        let PickOwner::Type(root_owner) = owners[i] else {
            return false;
        };
        let roots = types_of(owners[i]);
        !roots.is_empty()
            && (0..candidates.len()).all(|j| {
                owners[j] == owners[i]
                    || facts
                        .impl_member(candidates[j])
                        .and_then(|member| member.trait_owner)
                        == Some(root_owner)
                    || lineages[j].iter().any(|ty| roots.contains(ty))
            })
    })?;
    Some(candidates[root])
}

/// A parsed declaration, as [`rust_impl_members`] reads it.
pub trait ImplNode: HeritageNode {
    fn name(&self) -> &str;
    fn owner_class(&self) -> Option<&str>;
}

impl ImplNode for RawNode {
    fn name(&self) -> &str {
        &self.name
    }

    fn owner_class(&self) -> Option<&str> {
        self.owner_class.as_deref()
    }
}

/// The `impl` block of every Rust method in `nodes`, the declarations of
/// one file in source order: `note(member, block, implemented)` with
/// `member` and `block` offsets into `nodes` (the block by the offset of
/// any one of its nodes) and `implemented` the trait the block implements
/// as written (`None` for an inherent `impl`).
///
/// A method inside an `Impl` node's span belongs to the innermost such
/// block, which implements the trait the `Impl` node's owner names. The
/// parser emits no `Impl` node for a self type that is no plain or generic
/// type name (`impl T for &mut X`); a trait-impl method there still carries
/// the trait as its heritage. Such methods form one block per run in
/// source order of the same trait and owner, and a name the run already
/// holds starts the next block: one `impl` block never declares a name
/// twice. Two adjacent blocks of one trait with no shared name read as one
/// block.
pub fn rust_impl_members<'n, N: ImplNode>(
    nodes: &'n [N],
    mut note: impl FnMut(usize, usize, Option<&'n str>),
) {
    let contains = |outer: (u32, u32, u32, u32), inner: (u32, u32, u32, u32)| {
        (outer.0, outer.1) <= (inner.0, inner.1) && (outer.2, outer.3) >= (inner.2, inner.3)
    };
    let impls: Vec<usize> = (0..nodes.len())
        .filter(|&i| nodes[i].kind() == NodeKind::Impl)
        .collect();
    let mut orphans: Vec<usize> = Vec::new();
    for (offset, node) in nodes.iter().enumerate() {
        if !matches!(node.kind(), NodeKind::Method | NodeKind::Constructor) {
            continue;
        }
        let block = impls
            .iter()
            .copied()
            .filter(|&i| contains(nodes[i].span(), node.span()))
            .min_by_key(|&i| {
                let span = nodes[i].span();
                (span.2.saturating_sub(span.0), span.3.saturating_sub(span.1))
            });
        match block {
            Some(i) => note(offset, i, nodes[i].owner_class()),
            None if !node.heritage().is_empty() => orphans.push(offset),
            None => {}
        }
    }
    orphans.sort_by_key(|&i| (nodes[i].span().0, nodes[i].span().1));
    let mut run: Vec<&str> = Vec::new();
    let mut block = 0usize;
    let mut previous: Option<usize> = None;
    for offset in orphans {
        let node = &nodes[offset];
        let same_run = previous.is_some_and(|p| {
            nodes[p].heritage().first() == node.heritage().first()
                && nodes[p].owner_class() == node.owner_class()
        });
        if !same_run || run.contains(&node.name()) {
            run.clear();
            block = offset;
        }
        run.push(node.name());
        previous = Some(offset);
        note(offset, block, node.heritage().first().map(String::as_str));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of declarations: `(name, kind, owner, impl member)`, id = index.
    #[derive(Default)]
    struct Decls {
        decls: Vec<(
            &'static str,
            NodeKind,
            Option<&'static str>,
            Option<ImplMember<'static>>,
        )>,
        namespaces: Vec<&'static str>,
        /// `(type id, ancestors)`.
        ancestors: Vec<(u32, Vec<u32>)>,
    }

    impl PickFacts for Decls {
        fn pick_candidates(&self, _file: &str, name: &str) -> Cow<'_, [u32]> {
            Cow::Owned(
                (0..self.decls.len() as u32)
                    .filter(|&id| self.decls[id as usize].0 == name)
                    .collect(),
            )
        }
        fn kind(&self, id: u32) -> NodeKind {
            self.decls[id as usize].1
        }
        fn owner(&self, id: u32) -> Option<&str> {
            self.decls[id as usize].2
        }
        fn impl_member(&self, id: u32) -> Option<ImplMember<'_>> {
            self.decls[id as usize].3
        }
        fn is_namespace_owner(&self, _file: &str, owner: &str) -> bool {
            self.namespaces.contains(&owner)
        }
        fn file_of(&self, _id: u32) -> Option<&str> {
            Some("f")
        }
        fn types_named(&self, owner: &str) -> Vec<u32> {
            (0..self.decls.len() as u32)
                .filter(|&id| self.decls[id as usize].0 == owner && self.kind(id).is_type())
                .collect()
        }
        fn ancestors(&self, ty: u32) -> Vec<u32> {
            self.ancestors
                .iter()
                .find(|(t, _)| *t == ty)
                .map_or_else(Vec::new, |(_, a)| a.clone())
        }
    }

    const M: NodeKind = NodeKind::Method;
    const CALLABLE: ResolveTarget = ResolveTarget::Callable;

    fn untyped() -> PickScope {
        PickScope {
            untyped_receiver: true,
            ..PickScope::default()
        }
    }

    #[test]
    fn test_prefer_owned_two_unrelated_owners_returns_none() {
        let facts = Decls {
            decls: vec![("m", M, Some("A"), None), ("m", M, Some("B"), None)],
            ..Decls::default()
        };
        let pick = prefer_owned(
            &facts,
            "f",
            0,
            "m",
            None,
            &[],
            CALLABLE,
            PickScope::default(),
        );
        assert_eq!(pick, None);
    }

    #[test]
    fn test_prefer_owned_caller_owner_member_returns_own() {
        let facts = Decls {
            decls: vec![("m", M, Some("A"), None), ("m", M, Some("B"), None)],
            ..Decls::default()
        };
        let pick = prefer_owned(
            &facts,
            "f",
            0,
            "m",
            Some("B"),
            &[],
            CALLABLE,
            PickScope::default(),
        );
        assert_eq!(pick, Some(1));
    }

    #[test]
    fn test_prefer_owned_free_function_beats_unrelated_member_returns_free() {
        let facts = Decls {
            decls: vec![
                ("m", M, Some("A"), None),
                ("m", NodeKind::Function, None, None),
            ],
            ..Decls::default()
        };
        let pick = prefer_owned(
            &facts,
            "f",
            0,
            "m",
            Some("C"),
            &[],
            CALLABLE,
            PickScope::default(),
        );
        assert_eq!(pick, Some(1));
    }

    #[test]
    fn test_prefer_owned_namespace_owner_counts_as_free_returns_it() {
        let facts = Decls {
            decls: vec![
                ("m", M, Some("A"), None),
                ("m", NodeKind::Function, Some("ns"), None),
            ],
            namespaces: vec!["ns"],
            ..Decls::default()
        };
        let pick = prefer_owned(
            &facts,
            "f",
            0,
            "m",
            None,
            &[],
            CALLABLE,
            PickScope::default(),
        );
        assert_eq!(pick, Some(1));
    }

    #[test]
    fn test_prefer_owned_untyped_receiver_skips_free_and_own_type_returns_none() {
        let facts = Decls {
            decls: vec![
                ("m", NodeKind::Function, None, None),
                ("m", M, Some("A"), None),
            ],
            ..Decls::default()
        };
        assert_eq!(
            prefer_owned(&facts, "f", 0, "m", None, &[], CALLABLE, untyped()),
            Some(1)
        );
        assert_eq!(
            prefer_owned(&facts, "f", 0, "m", Some("A"), &[], CALLABLE, untyped()),
            None
        );
    }

    fn typed_type() -> PickScope {
        PickScope {
            typed_receiver: true,
            qualifier_is_type: true,
            ..PickScope::default()
        }
    }

    #[test]
    fn test_prefer_owned_type_receiver_unrelated_owner_or_free_returns_none() {
        let facts = Decls {
            decls: vec![
                ("Comment", NodeKind::Class, None, None),
                ("Article", NodeKind::Class, None, None),
                ("save", M, Some("Comment"), None),
                ("save", NodeKind::Function, None, None),
            ],
            ..Decls::default()
        };
        let pick = |scope| {
            prefer_owned(
                &facts,
                "f",
                2,
                "save",
                Some("Article"),
                &[],
                CALLABLE,
                scope,
            )
        };
        assert_eq!(pick(typed_type()), None);
        let module = PickScope {
            typed_receiver: true,
            ..PickScope::default()
        };
        assert_eq!(pick(module), Some(3));
    }

    #[test]
    fn test_prefer_owned_type_receiver_grandparent_member_returns_nearest_ancestor() {
        let facts = Decls {
            decls: vec![
                ("Base", NodeKind::Class, None, None),
                ("Mid", NodeKind::Class, None, None),
                ("Leaf", NodeKind::Class, None, None),
                ("Other", NodeKind::Class, None, None),
                ("m", M, Some("Other"), None),
                ("m", M, Some("Base"), None),
            ],
            ancestors: vec![(2, vec![1, 0])],
            ..Decls::default()
        };
        let pick = prefer_owned(
            &facts,
            "f",
            4,
            "m",
            Some("Leaf"),
            &[],
            CALLABLE,
            typed_type(),
        );
        assert_eq!(pick, Some(5));
    }

    /// `class D(B, C)`, `class B(A)`: Python's MRO is D, B, A, C, so `D.m`
    /// is `A.m`, while a breadth-first walk meets `C.m` first.
    #[test]
    fn test_prefer_owned_type_receiver_members_on_two_branches_returns_none() {
        let facts = Decls {
            decls: vec![
                ("A", NodeKind::Class, None, None),
                ("B", NodeKind::Class, None, None),
                ("C", NodeKind::Class, None, None),
                ("D", NodeKind::Class, None, None),
                ("m", M, Some("A"), None),
                ("m", M, Some("C"), None),
            ],
            ancestors: vec![(3, vec![1, 2, 0]), (1, vec![0])],
            ..Decls::default()
        };
        let pick = prefer_owned(&facts, "f", 5, "m", Some("D"), &[], CALLABLE, typed_type());
        assert_eq!(pick, None);
    }

    /// `class D(X, Y)`, `class Y(X)`: `Y.m` overrides `X.m` although the
    /// breadth-first walk meets `X` first.
    #[test]
    fn test_prefer_owned_type_receiver_override_on_one_chain_returns_overriding_member() {
        let facts = Decls {
            decls: vec![
                ("X", NodeKind::Class, None, None),
                ("Y", NodeKind::Class, None, None),
                ("D", NodeKind::Class, None, None),
                ("m", M, Some("X"), None),
                ("m", M, Some("Y"), None),
            ],
            ancestors: vec![(2, vec![0, 1]), (1, vec![0])],
            ..Decls::default()
        };
        let pick = prefer_owned(&facts, "f", 3, "m", Some("D"), &[], CALLABLE, typed_type());
        assert_eq!(pick, Some(4));
    }

    #[test]
    fn test_prefer_owned_impl_blocks_caller_block_returns_own_block_member() {
        let block = |block| {
            Some(ImplMember {
                block,
                trait_owner: Some("W"),
            })
        };
        let facts = Decls {
            decls: vec![
                ("m", M, Some("Cursor"), block(10)),
                ("run", M, Some("Cursor"), block(20)),
                ("m", M, Some("Cursor"), block(20)),
            ],
            ..Decls::default()
        };
        let scope = PickScope {
            caller: Some(1),
            typed_receiver: true,
            ..PickScope::default()
        };
        let pick = prefer_owned(
            &facts,
            "f",
            0,
            "m",
            Some("Cursor"),
            &[],
            ResolveTarget::Method,
            scope,
        );
        assert_eq!(pick, Some(2));
    }

    #[test]
    fn test_prefer_owned_base_and_derived_returns_hierarchy_root() {
        let facts = Decls {
            decls: vec![
                ("Base", NodeKind::Class, None, None),
                ("Derived", NodeKind::Class, None, None),
                ("m", M, Some("Derived"), None),
                ("m", M, Some("Base"), None),
            ],
            ancestors: vec![(1, vec![0])],
            ..Decls::default()
        };
        let pick = prefer_owned(&facts, "f", 2, "m", None, &[], CALLABLE, untyped());
        assert_eq!(pick, Some(3));
    }

    #[test]
    fn test_member_call_admits_free_namespace_own_or_constructor_returns_false() {
        let facts = Decls {
            decls: vec![
                ("f", NodeKind::Function, None, None),
                ("g", NodeKind::Function, Some("ns"), None),
                ("m", M, Some("A"), None),
                ("A", NodeKind::Constructor, Some("A"), None),
            ],
            namespaces: vec!["ns"],
            ..Decls::default()
        };
        assert!(!member_call_admits(&facts, 0, None));
        assert!(!member_call_admits(&facts, 1, None));
        assert!(member_call_admits(&facts, 2, None));
        assert!(!member_call_admits(&facts, 2, Some("A")));
        assert!(!member_call_admits(&facts, 3, None));
    }
}
