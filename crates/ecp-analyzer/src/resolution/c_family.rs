//! The C / C++ rule of the global (Tier 3) tier, over the index's candidates.
//! The session overlay holds the same rule over its own candidates
//! (`ecp_core::session::view::resolution::c_family_field`); the two must
//! agree, so a change here changes both.
//!
//! A `.h` file is C++ by extension (PR #142), so the language barrier of
//! [`FileMeta::admits`] keeps a C caller from every type and function its
//! headers declare. Two rules repair that without opening the barrier to
//! any other pair:
//!
//! - **Family fallback.** When no candidate of the caller's own language is
//!   a definition (none passes the barrier, or each that does is a
//!   prototype), a C caller also weighs the C++ candidates and a C++ caller
//!   the C candidates that both languages name the same way: a top-level
//!   (owner-less) function, struct, enum, typedef, variable, constant or
//!   macro. A class, a method and anything inside a namespace or a type
//!   stay C++ only. An own-language definition always wins, so a name that
//!   a C file and a C++ file both define keeps its old binding.
//! - **Declaration and definition are one target.** A C / C++ function
//!   declared without a body (a header prototype, see
//!   [`SymbolTable::note_c_prototype`](super::index::SymbolTable::note_c_prototype))
//!   leaves the field when a definition with the same owner is in it. Two
//!   definitions stay ambiguous.

use ecp_core::analyzer::same_file_pick::PickFacts;
use ecp_core::file_category::{FileMeta, Language};
use ecp_core::graph::NodeKind;
use rustc_hash::FxHashSet;

pub(crate) fn is_c_family(language: Language) -> bool {
    matches!(language, Language::C | Language::Cpp)
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

/// The Tier 3 field of a C or C++ `caller`, when the family rules change
/// it; `None` when `candidates` already is the field: the caller is no C /
/// C++ file, or some candidate passes the barrier and none of those is a
/// prototype. A crossing candidate carries the caller's language in its
/// meta, with its own vendor flag, so the ladder's barrier admits it and
/// the vendor barrier still applies.
pub(crate) fn c_family_field<F, I>(
    facts: &F,
    caller: FileMeta,
    candidates: impl Fn() -> I,
    is_prototype: impl Fn(u32) -> bool,
) -> Option<Vec<(u32, FileMeta)>>
where
    F: PickFacts + ?Sized,
    I: Iterator<Item = (u32, FileMeta)>,
{
    if !is_c_family(caller.language) {
        return None;
    }
    let (mut prototype, mut definition) = (false, false);
    for (id, _) in candidates().filter(|&(_, meta)| caller.admits(meta)) {
        if is_prototype(id) {
            prototype = true;
        } else {
            definition = true;
        }
    }
    if definition && !prototype {
        return None;
    }
    let mut field: Vec<(u32, FileMeta)> = candidates()
        .filter(|&(_, meta)| caller.admits(meta))
        .collect();
    if !definition {
        field.extend(
            candidates()
                .filter(|&(id, meta)| {
                    is_c_family(meta.language)
                        && meta.language != caller.language
                        && facts.owner(id).is_none()
                        && c_nameable(facts.kind(id))
                })
                .map(|(id, meta)| {
                    (
                        id,
                        FileMeta {
                            is_vendor: meta.is_vendor,
                            ..caller
                        },
                    )
                }),
        );
    }
    drop_defined_prototypes(facts, &mut field, &is_prototype);
    Some(field)
}

/// Removes each prototype whose owner also owns a definition in `field`.
fn drop_defined_prototypes<F: PickFacts + ?Sized>(
    facts: &F,
    field: &mut Vec<(u32, FileMeta)>,
    is_prototype: impl Fn(u32) -> bool,
) {
    // A C++ `void ns::f() {}` parses as a Method owned by `ns`, so a
    // definition of either callable kind settles `ns`'s prototype `f`.
    let defined: FxHashSet<Option<&str>> = field
        .iter()
        .filter(|&&(id, _)| {
            !is_prototype(id) && matches!(facts.kind(id), NodeKind::Function | NodeKind::Method)
        })
        .map(|&(id, _)| facts.owner(id))
        .collect();
    if !defined.is_empty() {
        field.retain(|&(id, _)| !(is_prototype(id) && defined.contains(&facts.owner(id))));
    }
}
