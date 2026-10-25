//! The receiver-typing ladder: `qualifier.member` resolved through the
//! member's owning type, its declared supertypes included, once the
//! qualifier names project types. Index-time resolution (`Resolver`) and the
//! session overlay find the candidate types each by their own import rules,
//! then read the ownership walk from here through [`TypeFacts`].

use crate::analyzer::reexport::ResolveTarget;
use crate::analyzer::same_file_pick::PickFacts;
use crate::analyzer::types::RawImport;
use crate::file_category::{FileMeta, Language};
use crate::graph::NodeKind;
use std::borrow::Cow;
use std::path::Path;

/// Depth cap of a supertype walk. Declared heritage deeper than this is
/// rare, and the cap bounds the walk on a corrupt or cyclic chain.
pub const MAX_HERITAGE_DEPTH: usize = 8;

/// The project types a qualifier can name (ladder step 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeCandidates<'q> {
    /// One type, with the name it is declared under (an import alias names
    /// the type differently from the qualifier).
    Unique(u32, &'q str),
    /// Several types, all declared under the one name carried.
    Set(Vec<u32>, &'q str),
    /// An import binds the qualifier to a file outside the project.
    External,
    None,
}

/// The result of resolving a typed chain step.
///
/// `Conclusive` suppresses the parser-name fallback. `Unknown` permits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainOutcome<T> {
    Bound(T),
    Conclusive,
    Unknown,
}

impl<T> ChainOutcome<T> {
    pub fn bound(self) -> Option<T> {
        match self {
            Self::Bound(value) => Some(value),
            Self::Conclusive | Self::Unknown => None,
        }
    }
}

/// Resolve an otherwise-unmodeled return type from project candidates.
///
/// A missing project type is conclusive. Several project types leave the
/// parser fallback available because no type identity is proven.
pub fn unmodeled_return_type<'q>(
    facts: &impl TypeFacts,
    source: &str,
    name: &'q str,
) -> ChainOutcome<TypeCandidates<'q>> {
    let candidates = facts.global_candidates(
        name,
        ResolveTarget::Type,
        FileMeta::from_normalized_path(source),
    );
    let mut candidates = distinct_types(facts, candidates.into_iter());
    match (candidates.next(), candidates.next()) {
        (None, None) => ChainOutcome::Conclusive,
        (Some(id), None) => ChainOutcome::Bound(TypeCandidates::Unique(id, name)),
        (Some(_), Some(_)) => ChainOutcome::Unknown,
        (None, Some(_)) => unreachable!("iterator cannot yield a second item first"),
    }
}

/// `candidates` with the parts of one C# `partial` type read as one type:
/// its representative ([`TypeFacts::partial_type`]), yielded once, at the
/// place of its first part.
fn distinct_types<'f, F: TypeFacts + ?Sized>(
    facts: &'f F,
    candidates: impl Iterator<Item = u32> + 'f,
) -> impl Iterator<Item = u32> + 'f {
    let mut seen: Vec<u32> = Vec::new();
    candidates.filter_map(move |id| {
        let ty = facts.partial_type(id);
        (!seen.contains(&ty)).then(|| {
            seen.push(ty);
            ty
        })
    })
}

/// Whether a type owns a member, directly or through its supertypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Owned {
        id: u32,
        inherited: bool,
    },
    NotOwned,
    /// The member may come from a base the index cannot see, or from one of
    /// several owners: no edge.
    Unknown,
}

/// The owner index's answer for one type and one member name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberLookup {
    Hit(u32),
    Miss,
    Ambiguous,
}

/// Outcome of the ladder for one qualified callee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeMember {
    /// The one candidate type declares the member (the qualifier tier's
    /// confidence).
    Owned(u32),
    /// The one candidate type inherits the member (the heritage tier's).
    Inherited(u32),
    /// Exactly one of several same-named types owns it (the global tier's).
    Candidate(u32),
    /// Copies of one type own it; the copy nearest the caller.
    NearestCopy(u32),
    /// Several candidate types could own the member: no edge.
    Ambiguous,
    NoEdge,
}

/// What the ladder reads besides the pick facts.
pub trait TypeFacts: PickFacts {
    /// `(bases, first_unresolved)` of the type `ty`: resolved bases in
    /// declared order, and the number of resolved bases declared before the
    /// first unresolved one. `None` when `ty` declares no heritage.
    fn supertypes(&self, ty: u32) -> Option<(Cow<'_, [u32]>, Option<u32>)>;
    /// The name `id` is declared under.
    fn name_of(&self, id: u32) -> Option<&str>;
    /// Members named `member` in `file` owned by `owner`, in source order,
    /// of `target`'s kinds.
    fn owned_in_file(
        &self,
        file: &str,
        member: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> Vec<u32>;
    /// Members named `member` owned by `owner` in the scope of `scope_file`:
    /// the same language, vendor barrier and crate root, and for Go the
    /// same directory.
    fn owned_global(
        &self,
        member: &str,
        owner: &str,
        target: ResolveTarget,
        scope_file: &str,
    ) -> Vec<u32>;
    /// Number of declarations named `name` in `file` of `target`'s kinds.
    fn count_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> usize;
    /// Declarations named `name` of `target`'s kinds that `caller` admits.
    fn global_candidates(&self, name: &str, target: ResolveTarget, caller: FileMeta) -> Vec<u32>;
    /// Whether `b` repeats declaration `a` in another source tree.
    fn is_copy(&self, a: u32, b: u32) -> bool;
    /// The one of `candidates`, copies of one declaration, nearest
    /// `caller_file`; `None` for a tie or when one is no copy.
    fn nearest_copy(&self, caller_file: &str, candidates: &[u32]) -> Option<u32>;
    /// The return type the function `callee` declares, as its signature
    /// writes it or already reduced by [`return_type_key`]; `None` when it
    /// declares none or is no function.
    fn return_type_of(&self, callee: u32) -> Option<&str>;
    /// Every part of `ty`'s C# `partial` type, the representative first,
    /// `ty` included, when the facts know the parts of one type; empty
    /// otherwise. A member another part declares is the type's own.
    fn partial_parts(&self, _ty: u32) -> Cow<'_, [u32]> {
        Cow::Borrowed(&[])
    }
    /// The node that stands for `ty`'s type: the representative part of a
    /// C# `partial` type, else `ty` itself. Two parts of one type are one
    /// candidate type.
    fn partial_type(&self, ty: u32) -> u32 {
        ty
    }
    /// The crate root of `path`: what precedes its last `/src/` or
    /// `/tests/` segment, else empty.
    fn crate_root<'p>(&self, path: &'p str) -> &'p str {
        path.rsplit_once("/src/")
            .or_else(|| path.rsplit_once("/tests/"))
            .map_or("", |(root, _)| root)
    }
}

/// Whether a return type names the type that declares the function: Rust
/// and Swift `Self`, PHP `self` / `static`, TypeScript `this`.
fn names_declaring_type(key: &str) -> bool {
    matches!(key, "Self" | "self" | "static" | "this")
}

/// The type name a declared return type reduces to when only wrappers that
/// keep the type's members surround it: a leading `:` / `->` an extractor
/// kept, Python string quotes (`"Foo"`),
/// a union with a null (`Foo | null`, `Foo | undefined`, `Foo | None`), a
/// nullable marker (`?Foo`, `Foo?`, `Foo!`), a pointer or reference
/// (`*Foo`, `&Foo`, `&mut Foo`, `&'a Foo`, `Foo*`, `Foo&`), `const`, and a C
/// `struct` / C++ `class` tag. `None` for everything else: a generic or an
/// array (`Promise<Foo>`, `Optional<Foo>`, `Vec<Foo>`, `list[Foo]`,
/// `Foo[]`), a qualified path, a tuple, a function type, a union of two
/// real types. Those need their own unwrapping rules (a follow-up), and a
/// guess would bind the wrong member.
pub fn return_type_key(raw: &str) -> Option<&str> {
    let mut ty = unquote(raw.trim());
    if ty.contains('|') {
        let mut kept = ty
            .split('|')
            .map(str::trim)
            .filter(|part| !matches!(*part, "null" | "undefined" | "None" | "nil"));
        ty = kept.next()?;
        if kept.next().is_some() {
            return None;
        }
    }
    loop {
        let before = ty;
        ty = ty.trim();
        for prefix in [
            "->", ":", "?", "*", "&", "const ", "mut ", "struct ", "class ",
        ] {
            ty = ty.strip_prefix(prefix).unwrap_or(ty).trim_start();
        }
        if let Some(lifetimed) = ty.strip_prefix('\'') {
            ty = lifetimed
                .split_once(char::is_whitespace)
                .map_or("", |(_, rest)| rest);
        }
        for suffix in ["?", "!", "*", "&", " const"] {
            ty = ty.strip_suffix(suffix).unwrap_or(ty).trim_end();
        }
        if ty == before {
            break;
        }
    }
    let names_a_type = !ty.is_empty()
        && !matches!(ty, "null" | "undefined" | "None" | "nil" | "void")
        && ty
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    names_a_type.then_some(ty)
}

/// `text` without one pair of surrounding `"` or `'` quotes.
fn unquote(text: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.trim();
        }
    }
    text
}

/// The name of the type a call to `callee` returns, as the callee's own
/// file names it: a constructor's owner, a constructible type itself (the
/// constructor fallback lands on a type that declares no constructor), or
/// the function's declared return type reduced by [`return_type_key`],
/// where `Self` / `self` / `static` / `this` stand for the declaring type.
/// The receiver of a chained call (`Foo.create().bar()`) has this type.
pub fn returned_type<F: TypeFacts + ?Sized>(facts: &F, callee: u32) -> Option<&str> {
    match facts.kind(callee) {
        NodeKind::Constructor => facts.owner(callee),
        kind if kind.is_constructible() => facts.name_of(callee),
        NodeKind::Function | NodeKind::Method => {
            let key = return_type_key(facts.return_type_of(callee)?)?;
            if names_declaring_type(key) {
                facts.owner(callee)
            } else {
                Some(key)
            }
        }
        _ => None,
    }
}

/// Select a visible return type before inspecting its members. An ambiguous
/// type remains ambiguous even when only one candidate declares the member.
pub fn visible_return_type<'q, F: TypeFacts + ?Sized>(
    facts: &F,
    source: &str,
    name: &'q str,
    mut visible: impl FnMut(u32) -> bool,
) -> TypeCandidates<'q> {
    let candidates = facts.global_candidates(
        name,
        ResolveTarget::Type,
        FileMeta::from_normalized_path(source),
    );
    let mut candidates = distinct_types(facts, candidates.into_iter().filter(|&id| visible(id)));
    match (candidates.next(), candidates.next()) {
        (Some(id), None) => TypeCandidates::Unique(id, name),
        _ => TypeCandidates::None,
    }
}

/// Namespace visible from a type's owner key. A file-scoped namespace has
/// no stamped owner, so it requires one namespace in the file. Two full
/// namespace names with one owner key cannot be distinguished.
pub fn declared_namespace<'a>(
    owner: Option<&str>,
    namespaces: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let mut names = namespaces.filter(|name| {
        owner.is_none_or(|owner| crate::analyzer::types::owner_key(name) == Some(owner))
    });
    match (names.next(), names.next()) {
        (None, None) if owner.is_none() => Some(""),
        (Some(name), None) => Some(name),
        _ => None,
    }
}

/// Whether a C# type belongs to the global namespace, the callee's one
/// namespace, or an explicitly imported namespace.
pub fn namespace_return_type_visible<'a>(
    owner: Option<&str>,
    namespaces: impl Iterator<Item = &'a str>,
    source_namespaces: impl Iterator<Item = &'a str>,
    imports: &[RawImport],
) -> bool {
    let Some(namespace) = declared_namespace(owner, namespaces) else {
        return false;
    };
    namespace.is_empty()
        || declared_namespace(None, source_namespaces) == Some(namespace)
        || imports.iter().any(|import| {
            import.alias.is_none()
                && import.source == namespace
                && import.imported_name == namespace
        })
}

/// Whether a type's namespace agrees with a C# alias's full type path.
pub fn namespace_type_path_matches<'a>(
    owner: Option<&str>,
    namespaces: impl Iterator<Item = &'a str>,
    path: &str,
) -> bool {
    let path = path.strip_prefix("global::").unwrap_or(path);
    let namespace = path.rsplit_once('.').map_or("", |(namespace, _)| namespace);
    declared_namespace(owner, namespaces) == Some(namespace)
}

/// A Swift package target's conventional `Sources/<module>/` directory.
/// Custom target paths have no module evidence here.
fn swift_module(file: &str) -> Option<(&str, &str)> {
    let (root, tail) = file
        .rsplit_once("/Sources/")
        .or_else(|| file.strip_prefix("Sources/").map(|tail| ("", tail)))?;
    let (module, _) = tail.split_once('/')?;
    Some((root, module))
}

/// A Swift return type is visible in its own target or an imported target
/// of the same package. Without target layout evidence, preserve directory
/// scope rather than guessing a module from a global namesake.
pub fn swift_return_type_visible(source: &str, candidate: &str, imports: &[RawImport]) -> bool {
    match (swift_module(source), swift_module(candidate)) {
        (Some((root, module)), Some((other_root, other_module))) => {
            root == other_root
                && (module == other_module
                    || imports
                        .iter()
                        .any(|import| import.alias.is_none() && import.source == other_module))
        }
        _ => Path::new(source).parent() == Path::new(candidate).parent(),
    }
}

/// Languages where a field or class attribute hides an inherited method of
/// the same name: Python class attributes, JavaScript / TypeScript instance
/// fields over prototype methods, and Go fields over promoted methods. Java,
/// Kotlin, C#, Rust and the rest keep fields and methods apart.
pub fn attributes_shadow_methods(meta: FileMeta) -> bool {
    matches!(
        meta.language,
        Language::Python | Language::JavaScript | Language::TypeScript | Language::Go
    )
}

/// The ladder past step 1: resolve `member` of the types `candidates`
/// names. It emits an edge only when that type or one of its resolved
/// declared supertypes owns the member, and no unresolved base can shadow
/// the owner. It never matches the member name alone. `call` admits the
/// nearest-copy pick among several owning types.
pub fn member_via_type<F: TypeFacts + ?Sized>(
    facts: &F,
    candidates: TypeCandidates<'_>,
    caller_file: &str,
    member: &str,
    target: ResolveTarget,
    call: bool,
) -> TypeMember {
    match candidates {
        TypeCandidates::Unique(ty, ty_name) => {
            match member_ownership(facts, ty, ty_name, member, target) {
                Ownership::Owned {
                    id,
                    inherited: false,
                } => TypeMember::Owned(id),
                Ownership::Owned {
                    id,
                    inherited: true,
                } => TypeMember::Inherited(id),
                Ownership::NotOwned | Ownership::Unknown => TypeMember::NoEdge,
            }
        }
        TypeCandidates::Set(types, ty_name) => {
            let mut owner_members: Vec<u32> = Vec::new();
            for ty in types {
                match member_ownership(facts, ty, ty_name, member, target) {
                    Ownership::Owned { id, .. } => match owner_members.first() {
                        Some(_) if owner_members.contains(&id) => {}
                        Some(&first) if !call || !facts.is_copy(first, id) => {
                            return TypeMember::Ambiguous;
                        }
                        _ => owner_members.push(id),
                    },
                    Ownership::NotOwned => {}
                    Ownership::Unknown => return TypeMember::Ambiguous,
                }
            }
            match owner_members.as_slice() {
                [] => TypeMember::NoEdge,
                &[id] => TypeMember::Candidate(id),
                owners => match facts.nearest_copy(caller_file, owners) {
                    Some(id) => TypeMember::NearestCopy(id),
                    None => TypeMember::Ambiguous,
                },
            }
        }
        TypeCandidates::External | TypeCandidates::None => TypeMember::NoEdge,
    }
}

/// Ladder steps 2 and 3: does type `ty` own `member`, directly or through
/// its resolved declared supertypes?
///
/// The walk visits every ancestor (breadth-first, declared order, depth
/// [`MAX_HERITAGE_DEPTH`]). The owner is the most derived owning type; two
/// owners on separate branches depend on the language's method resolution
/// order, so they give `Unknown`. The result is also `Unknown` when
/// something the lookup may meet before the owner could supply the member
/// instead: an unresolved base, or a same-named attribute (`greet =
/// replacement`) where attributes hide methods, on the path to the owner,
/// on a branch declared before the owner's, or on a branch that also
/// reaches the owner.
pub fn member_ownership<F: TypeFacts + ?Sized>(
    facts: &F,
    ty: u32,
    ty_name: &str,
    member: &str,
    target: ResolveTarget,
) -> Ownership {
    match owned_member(facts, ty, ty_name, member, target) {
        MemberLookup::Hit(id) => {
            return Ownership::Owned {
                id,
                inherited: false,
            }
        }
        MemberLookup::Ambiguous => return Ownership::Unknown,
        MemberLookup::Miss => {}
    }
    let shadows =
        attributes_shadow_methods(FileMeta::from_path(facts.file_of(ty).unwrap_or_default()));
    // How each ancestor was first reached: (the type, base position).
    let mut parent: rustc_hash::FxHashMap<u32, (u32, u32)> = rustc_hash::FxHashMap::default();
    let mut frontier = vec![ty];
    // (type, number of resolved bases it declares before an unresolved one)
    let mut unresolved: Vec<(u32, u32)> = Vec::new();
    // Types declaring a same-named attribute, which hides a method.
    let mut attributes: Vec<u32> = Vec::new();
    if shadows && declares_attribute(facts, ty, ty_name, member) {
        attributes.push(ty);
    }
    let mut hits: Vec<(u32, u32)> = Vec::new();
    for _ in 0..MAX_HERITAGE_DEPTH {
        let mut next = Vec::new();
        for &current in &frontier {
            let Some((bases, first_unresolved)) = facts.supertypes(current) else {
                continue;
            };
            if let Some(at) = first_unresolved {
                unresolved.push((current, at));
            }
            for (pos, &base) in bases.iter().enumerate() {
                if base == ty || parent.contains_key(&base) {
                    continue;
                }
                parent.insert(base, (current, pos as u32));
                next.push(base);
                let Some(base_name) = facts.name_of(base) else {
                    continue;
                };
                if shadows && declares_attribute(facts, base, base_name, member) {
                    attributes.push(base);
                }
                match owned_member(facts, base, base_name, member, target) {
                    MemberLookup::Hit(id) => hits.push((base, id)),
                    MemberLookup::Ambiguous => return Ownership::Unknown,
                    MemberLookup::Miss => {}
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    // The depth cap cut the walk short: a deeper base may own the member.
    if frontier.iter().any(|&t| facts.supertypes(t).is_some()) {
        return Ownership::Unknown;
    }
    let most_derived = hits.iter().find(|&&(owner, _)| {
        let above = facts.ancestors(owner);
        hits.iter()
            .all(|&(other, _)| other == owner || above.contains(&other))
    });
    let Some(&(owner, id)) = most_derived else {
        return if hits.is_empty() && unresolved.is_empty() {
            Ownership::NotOwned
        } else {
            Ownership::Unknown
        };
    };
    // The walk path from `ty` to the owner: each type on it, with the
    // position of the base it takes toward the owner.
    let mut path: rustc_hash::FxHashMap<u32, u32> = rustc_hash::FxHashMap::default();
    let mut node = owner;
    while let Some(&(up, pos)) = parent.get(&node) {
        path.insert(up, pos);
        node = up;
    }
    let above_owner = facts.ancestors(owner);
    // Does lookup meet `holder`'s contribution before the owner? `slot` is
    // an unresolved base of `holder` with `at` resolved bases before it;
    // `None` is a member `holder` declares itself. Anything above the owner
    // is hidden by it. Off the path, a holder that also reaches the owner
    // puts the owner after its own bases (a diamond: C3 visits a shared
    // ancestor last); otherwise its branch comes first when declared before
    // the branch the path takes.
    let before_owner = |holder: u32, slot: Option<u32>| {
        if holder == owner || above_owner.contains(&holder) {
            return false;
        }
        let (mut node, mut pos) = match (path.get(&holder), slot) {
            (Some(&taken), Some(at)) => return at <= taken,
            (Some(_), None) => return true,
            (None, _) if facts.ancestors(holder).contains(&owner) => return true,
            (None, _) => match parent.get(&holder) {
                Some(&(up, at)) => (up, at),
                None => return true,
            },
        };
        loop {
            if let Some(&taken) = path.get(&node) {
                return pos < taken;
            }
            let Some(&(up, at)) = parent.get(&node) else {
                return true;
            };
            (node, pos) = (up, at);
        }
    };
    if unresolved.iter().any(|&(h, at)| before_owner(h, Some(at)))
        || attributes.iter().any(|&h| before_owner(h, None))
    {
        return Ownership::Unknown;
    }
    Ownership::Owned {
        id,
        inherited: true,
    }
}

/// True when type `ty` declares a non-callable member named `member` (a
/// field or a class attribute), which hides an inherited method in the
/// languages [`attributes_shadow_methods`] lists.
fn declares_attribute<F: TypeFacts + ?Sized>(
    facts: &F,
    ty: u32,
    ty_name: &str,
    member: &str,
) -> bool {
    facts.file_of(ty).is_some_and(|file| {
        !facts
            .owned_in_file(file, member, ty_name, ResolveTarget::Field)
            .is_empty()
    })
}

/// The member of `ty` named `member`, by the owner index: first in the
/// type's own file, then in the type's crate / package scope.
pub fn owned_member<F: TypeFacts + ?Sized>(
    facts: &F,
    ty: u32,
    ty_name: &str,
    member: &str,
    target: ResolveTarget,
) -> MemberLookup {
    let Some(ty_file) = facts.file_of(ty) else {
        return MemberLookup::Miss;
    };
    let owner_target = if FileMeta::from_path(ty_file).language == Language::Go {
        ResolveTarget::Type
    } else {
        ResolveTarget::Qualifier
    };
    let declares_owner = |file: &str| facts.count_in_file(file, ty_name, owner_target);
    // The type's own file first, then the files of its other C# partial
    // parts: a member any part declares is the type's own.
    let parts = facts.partial_parts(ty);
    let part_files = parts
        .iter()
        .filter_map(|&part| facts.file_of(part))
        .filter(|&file| file != ty_file);
    for file in std::iter::once(ty_file).chain(part_files) {
        let in_file = facts.owned_in_file(file, member, ty_name, target);
        if let Some(&first) = in_file.first() {
            // Several hits are overloads of the one type, unless the file
            // declares two owners of that name (nested types, a type and a
            // module); the owner index cannot tell those apart.
            return if in_file.len() == 1 || declares_owner(file) == 1 {
                MemberLookup::Hit(first)
            } else {
                MemberLookup::Ambiguous
            };
        }
    }
    let elsewhere = facts.owned_global(member, ty_name, target, ty_file);
    let Some(&first) = elsewhere.first() else {
        return MemberLookup::Miss;
    };
    if type_unique_in_scope(facts, ty_name, ty_file) {
        return MemberLookup::Hit(first);
    }
    // Another same-named type shares the scope. Where a type can be
    // reopened in another file (Ruby `class Derived`, a C# partial class, a
    // Swift / Dart extension), a member there may be this type's own;
    // elsewhere a member in a file declaring that name belongs to that
    // other type, and any other member cannot be attributed.
    let reopenable = matches!(
        FileMeta::from_path(ty_file).language,
        Language::Ruby | Language::Crystal | Language::CSharp | Language::Swift | Language::Dart
    );
    if !reopenable
        && elsewhere
            .iter()
            .all(|&id| facts.file_of(id).is_some_and(|f| declares_owner(f) > 0))
    {
        MemberLookup::Miss
    } else {
        MemberLookup::Ambiguous
    }
}

/// True when `ty_file`'s type `ty_name` is the only qualifier of that name
/// in the scope [`TypeFacts::owned_global`] searches: the same language and
/// vendor barrier and crate root, and for Go one package.
pub fn type_unique_in_scope<F: TypeFacts + ?Sized>(
    facts: &F,
    ty_name: &str,
    ty_file: &str,
) -> bool {
    let meta = FileMeta::from_path(ty_file);
    let root = facts.crate_root(ty_file);
    let dir = Path::new(ty_file).parent();
    let same_package_only = meta.language == Language::Go;
    let owner_target = if same_package_only {
        ResolveTarget::Type
    } else {
        ResolveTarget::Qualifier
    };
    facts
        .global_candidates(ty_name, owner_target, meta)
        .into_iter()
        .filter(|&id| {
            facts.file_of(id).is_some_and(|f| {
                facts.crate_root(f) == root && (!same_package_only || Path::new(f).parent() == dir)
            })
        })
        .take(2)
        .count()
        == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract: only wrappers that keep the type's members reduce; a
    /// generic, an array, a path or a two-type union stays unresolved (the
    /// follow-up), so no chained call guesses its receiver.
    #[test]
    fn test_return_type_key_wrappers_and_generics_returns_bare_name_or_none() {
        for (raw, want) in [
            ("Foo", Some("Foo")),
            ("  Foo ", Some("Foo")),
            (": Foo", Some("Foo")),
            ("-> Foo", Some("Foo")),
            ("\"Foo\"", Some("Foo")),
            ("'Foo'", Some("Foo")),
            ("Foo | null", Some("Foo")),
            ("undefined | Foo", Some("Foo")),
            ("Foo | None", Some("Foo")),
            ("\"Foo | None\"", Some("Foo")),
            ("?Foo", Some("Foo")),
            ("Foo?", Some("Foo")),
            ("Foo!", Some("Foo")),
            ("*Foo", Some("Foo")),
            ("&Foo", Some("Foo")),
            ("&mut Self", Some("Self")),
            ("&'a Foo", Some("Foo")),
            ("const Foo&", Some("Foo")),
            ("Foo*", Some("Foo")),
            ("struct Foo", Some("Foo")),
            ("*const Foo", Some("Foo")),
            ("static", Some("static")),
            ("Promise<Foo>", None),
            ("Optional<Foo>", None),
            ("Vec<Foo>", None),
            ("list[Foo]", None),
            ("Foo[]", None),
            ("Foo | Bar", None),
            ("ns::Foo", None),
            ("pkg.Foo", None),
            ("(Foo, error)", None),
            ("() => Foo", None),
            ("", None),
            ("null", None),
            ("void", None),
        ] {
            assert_eq!(return_type_key(raw), want, "return_type_key({raw:?})");
        }
    }

    /// Contract: the reduced key is a fixed point, so the index (which
    /// stores the key) and the overlay (which reduces the raw text at query
    /// time) read the same type.
    #[test]
    fn test_return_type_key_reduced_key_returns_itself() {
        for raw in ["Foo", "?Foo", "&mut Self", "struct Foo", "\"Foo | None\""] {
            let key = return_type_key(raw).unwrap();
            assert_eq!(return_type_key(key), Some(key), "{raw:?}");
        }
    }
}
