//! The C / C++ rule of the global (Tier 3) tier. The index
//! (`ecp-analyzer` `SymbolTable::c_family_field`) and the session overlay
//! (`session/view/resolution.rs`) both run [`c_family_field`] over their own
//! candidates, so a dirty file binds what a reindex binds.
//!
//! A `.h` file is C++ by extension (PR #142), so the language barrier of
//! [`FileMeta::admits`] keeps a C caller from every type and function its
//! headers declare. Two rules repair that without opening the barrier to
//! any other pair:
//!
//! - **Family fallback.** When the caller's own language leaves a prototype
//!   that none of its definitions settles, or offers no candidate at all, a
//!   C caller also weighs the C++ candidates and a C++ caller the C
//!   candidates that both languages name the same way: a top-level
//!   (owner-less) function, struct, enum, typedef, variable, constant or
//!   macro. A class, a method and anything inside a namespace or a type
//!   stay C++ only. A `static` function has internal linkage, so it never
//!   crosses, and a vendor one never crosses to a non-vendor caller. Beside
//!   an owner-less prototype, a crossing definition of another parameter
//!   list is another overload, not the declared function, so it stays out.
//!   An own-language definition with no prototype beside it
//!   always wins, so a name that a C file and a C++ file both define keeps
//!   its old binding.
//! - **Declaration and definition are one target.** A C / C++ function
//!   declared without a body (a header prototype: `FunctionMeta::FLAG_EXTERN`
//!   on a `Function`) leaves the field when a definition in it settles it:
//!   the same owner, the same parameter types, and a definition the caller
//!   can reach (no vendor one for a non-vendor caller, no `static` one in
//!   another file). Two definitions stay ambiguous, and so does a prototype
//!   that no definition settles. A callable whose parse paired no meta, or
//!   whose meta lacks a parameter's type (a K&R name without its
//!   declaration), has no known parameter types: it settles nothing.

use crate::analyzer::same_file_pick::PickFacts;
use crate::file_category::{FileMeta, Language};
use crate::graph::{FunctionMeta, NodeKind};
use rustc_hash::{FxHashSet, FxHasher};
use std::hash::{Hash, Hasher};

pub fn is_c_family(language: Language) -> bool {
    matches!(language, Language::C | Language::Cpp)
}

/// What the `FunctionMeta` of a C or C++ `Function` / `Method` tells the
/// rule. A callable whose parse paired no meta has none.
#[derive(Debug, Clone, Copy)]
pub struct CCallable {
    flags: u16,
    /// The parameter count: `FunctionMeta::params` holds a name and a type
    /// per parameter. `f(void)` and `f()` both count 0, a `...` counts 1.
    arity: u16,
    /// A hash of the normalised parameter types ([`hash_type`]); `None`
    /// when a parameter other than `...` has no type text.
    signature: Option<u64>,
}

impl CCallable {
    /// The facts of a callable whose meta carries `flags` and the flat
    /// `params` list `[name1, type1, name2, type2, ..]`.
    pub fn new<'s>(flags: u16, params: impl IntoIterator<Item = &'s str>) -> Self {
        let mut hasher = FxHasher::default();
        let mut known = true;
        let mut name = "";
        let mut len = 0usize;
        for text in params {
            if len % 2 == 0 {
                name = text;
            } else if !text.trim().is_empty() {
                hash_type(&mut hasher, text);
            } else if name == "..." {
                hash_type(&mut hasher, name);
            } else {
                known = false;
            }
            len += 1;
        }
        Self {
            flags,
            arity: u16::try_from(len / 2).unwrap_or(u16::MAX),
            signature: known.then(|| hasher.finish()),
        }
    }

    fn is_static(self) -> bool {
        self.flags & FunctionMeta::FLAG_STATIC != 0
    }
}

/// Feed the parameter type `ty` to `hasher` in one spelling: whitespace
/// kept only between two identifier characters (`T *` == `T*`), and a
/// leading elaborated keyword dropped (`struct foo` == `foo`), so a C
/// header and a C++ definition of one function agree.
fn hash_type(hasher: &mut FxHasher, ty: &str) {
    let ty = ty.trim();
    let ty = ["struct ", "enum ", "union ", "class "]
        .iter()
        .find_map(|keyword| ty.strip_prefix(keyword))
        .map_or(ty, str::trim_start);
    let (mut gap, mut prev_ident) = (false, false);
    for c in ty.chars() {
        if c.is_whitespace() {
            gap = true;
            continue;
        }
        let ident = c.is_alphanumeric() || c == '_';
        if gap && prev_ident && ident {
            ' '.hash(hasher);
        }
        c.hash(hasher);
        (gap, prev_ident) = (false, ident);
    }
    // Ends the parameter, so `(a, bc)` and `(ab, c)` differ.
    hasher.write_u8(0xff);
}

/// A prototype's or a definition's owner, parameter count and parameter
/// types: a definition settles the prototype with the same key when the
/// types are known.
type Key<'f> = (Option<&'f str>, u16, Option<u64>);

/// Whether a definition keyed `definition` may be the function the
/// prototype keyed `prototype` declares: the same owner and count, and no
/// known pair of parameter lists that differ.
fn may_declare(prototype: Key<'_>, definition: Key<'_>) -> bool {
    prototype.0 == definition.0
        && prototype.1 == definition.1
        && match (prototype.2, definition.2) {
            (Some(declared), Some(defined)) => declared == defined,
            _ => true,
        }
}

/// The key of the prototype `id`; `None` when `id` is no prototype.
fn prototype_key<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    callable: &impl Fn(u32) -> Option<CCallable>,
    id: u32,
) -> Option<Key<'f>> {
    if facts.kind(id) != NodeKind::Function {
        return None;
    }
    let meta = callable(id).filter(|meta| meta.flags & FunctionMeta::FLAG_EXTERN != 0)?;
    Some((facts.owner(id), meta.arity, meta.signature))
}

/// The key of the definition `id`, a function or method with a body. A
/// C++ `void ns::f() {}` parses as a Method owned by `ns`, so either kind
/// settles `ns`'s `f`. `None` for a prototype, any other node, and a
/// callable without meta.
fn definition_key<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    callable: &impl Fn(u32) -> Option<CCallable>,
    id: u32,
) -> Option<Key<'f>> {
    let kind = facts.kind(id);
    if !matches!(kind, NodeKind::Function | NodeKind::Method) {
        return None;
    }
    let meta = callable(id)?;
    (kind == NodeKind::Method || meta.flags & FunctionMeta::FLAG_EXTERN == 0)
        .then(|| (facts.owner(id), meta.arity, meta.signature))
}

/// The keys the definitions in `entries` settle for a caller in
/// `caller_file`. A definition the caller cannot reach settles nothing, or
/// the prototype would leave and the barrier then drop the definition too.
/// Neither does one whose parameter types are unknown.
fn settled_keys<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    caller: FileMeta,
    caller_file: &str,
    entries: &[(u32, FileMeta)],
    callable: &impl Fn(u32) -> Option<CCallable>,
) -> FxHashSet<Key<'f>> {
    entries
        .iter()
        .filter(|&&(id, meta)| {
            caller.admits(meta)
                && (!callable(id).is_some_and(CCallable::is_static)
                    || facts.file_of(id) == Some(caller_file))
        })
        .filter_map(|&(id, _)| definition_key(facts, callable, id))
        .filter(|&(_, _, signature)| signature.is_some())
        .collect()
}

/// The Tier 3 field of a C or C++ caller in `caller_file`, when the family
/// rules change it; `None` when `candidates` already is the field: the
/// caller is no C / C++ file, or some candidate passes the barrier and none
/// of those is a prototype. `callable` reads [`CCallable`] for a C / C++
/// `Function` or `Method`. A crossing candidate carries the caller's
/// language in its meta, with its own vendor flag, so the ladder's barrier
/// admits it and the vendor barrier still applies.
pub fn c_family_field<F, I>(
    facts: &F,
    caller: FileMeta,
    caller_file: &str,
    candidates: impl Fn() -> I,
    callable: impl Fn(u32) -> Option<CCallable>,
) -> Option<Vec<(u32, FileMeta)>>
where
    F: PickFacts + ?Sized,
    I: Iterator<Item = (u32, FileMeta)>,
{
    if !is_c_family(caller.language) {
        return None;
    }
    let mut field: Vec<(u32, FileMeta)> = candidates()
        .filter(|&(_, meta)| caller.admits(meta))
        .collect();
    let mut prototypes: FxHashSet<Key<'_>> = field
        .iter()
        .filter_map(|&(id, _)| prototype_key(facts, &callable, id))
        .collect();
    if !field.is_empty() && prototypes.is_empty() {
        return None;
    }
    let mut settled = settled_keys(facts, caller, caller_file, &field, &callable);
    if !prototypes.is_subset(&settled) || field.is_empty() {
        let crossing: Vec<(u32, FileMeta)> = candidates()
            .filter(|&(id, meta)| {
                is_c_family(meta.language)
                    && meta.language != caller.language
                    && facts.owner(id).is_none()
                    && c_nameable(facts.kind(id))
                    && !callable(id).is_some_and(CCallable::is_static)
            })
            .map(|(id, meta)| {
                (
                    id,
                    FileMeta {
                        is_vendor: meta.is_vendor,
                        ..caller
                    },
                )
            })
            // The vendor barrier before the prototypes: a vendor prototype
            // the caller cannot reach must not push a reachable definition
            // out as another overload.
            .filter(|&(_, meta)| caller.admits(meta))
            .collect();
        prototypes.extend(
            crossing
                .iter()
                .filter_map(|&(id, _)| prototype_key(facts, &callable, id)),
        );
        // A crossing definition is owner-less, so only an owner-less
        // prototype declares it.
        let declared = prototypes.iter().any(|&(owner, _, _)| owner.is_none());
        let own = field.len();
        field.extend(crossing.into_iter().filter(|&(id, _)| {
            !declared
                || definition_key(facts, &callable, id).is_none_or(|key| {
                    prototypes
                        .iter()
                        .any(|&prototype| may_declare(prototype, key))
                })
        }));
        settled.extend(settled_keys(
            facts,
            caller,
            caller_file,
            &field[own..],
            &callable,
        ));
    }
    if !settled.is_empty() {
        field.retain(|&(id, _)| {
            prototype_key(facts, &callable, id).is_none_or(|key| !settled.contains(&key))
        });
    }
    Some(field)
}

/// The kinds a C declaration and a C++ declaration share.
fn c_nameable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Typedef
            | NodeKind::Variable
            | NodeKind::Const
            | NodeKind::Macro
    )
}
