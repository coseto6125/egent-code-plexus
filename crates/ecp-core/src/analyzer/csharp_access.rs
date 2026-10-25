//! C# member accessibility for call binding (codex ruling P-ext2, part C).
//!
//! A call binds a C# method only when C# lets the caller reach it. An
//! instance call (`o.M()`, `this.M()`) never reaches a static method other
//! than an extension method. A `private` member is reachable only from its
//! own type and the types nested in it. A fact the facts cannot show admits
//! the edge: the rule removes an edge only on proof. Index-time resolution
//! and the session overlay read this one rule.
//!
//! Not modeled: `protected` and `internal`. A call on a value of unknown
//! type (`object o; o.M()`) binds no ordinary target the rule could judge
//! (codex ruling P-recv Q3); only an extension method, which it admits.

use crate::analyzer::csharp_extension::EXTENSION_FLAG;
use crate::analyzer::same_file_pick::PickFacts;
use crate::analyzer::types::CallSite;
use crate::graph::{FunctionMeta, NodeKind};

/// `FunctionMeta::flags` visibility code of `private` (bits 6-8).
const PRIVATE: u16 = 2;

/// How deep a caller's owner chain is walked. Past it the chain proves
/// nothing.
const NESTING_LIMIT: usize = 32;

/// The call one site makes, as the access rule reads it.
#[derive(Debug, Clone, Copy)]
pub struct CallAccess<'a> {
    /// The site calls through an instance receiver (a typed `o.M()`).
    pub instance: bool,
    /// The caller's file.
    pub caller_file: &'a str,
    /// The `owner_key` of the type that declares the caller.
    pub caller_owner: Option<&'a str>,
}

impl<'a> CallAccess<'a> {
    /// The access of `site`, called from `caller_owner` in `caller_file`.
    /// A typed member site (`Type.member`) and an untyped member call go
    /// through an instance; a `Name.M()` whose receiver may be a type does
    /// not.
    pub fn of(site: CallSite<'_>, caller_file: &'a str, caller_owner: Option<&'a str>) -> Self {
        let instance = match site.fallback() {
            CallSite::Plain(name) => name.contains('.'),
            CallSite::UntypedMember(_) => true,
            _ => false,
        };
        Self {
            instance,
            caller_file,
            caller_owner,
        }
    }
}

/// Whether C# lets `call` reach the method `target`, whose
/// `FunctionMeta::flags` are `flags`. Any target other than a method is
/// admitted.
pub fn call_admits<F: PickFacts + ?Sized>(
    facts: &F,
    target: u32,
    flags: u16,
    call: CallAccess<'_>,
) -> bool {
    if facts.kind(target) != NodeKind::Method {
        return true;
    }
    let is_static = flags & FunctionMeta::FLAG_STATIC != 0;
    if call.instance && is_static && flags & EXTENSION_FLAG == 0 {
        return false;
    }
    if (flags >> 6) & 0b111 != PRIVATE {
        return true;
    }
    let Some((target_file, owner)) = facts.file_of(target).zip(facts.owner(target)) else {
        return true;
    };
    // A member of a namespace, or of a type the facts cannot place, proves
    // nothing. An interface member is public whatever its flags say.
    match type_in(facts, target_file, owner) {
        Some(owner_type) if facts.kind(owner_type) != NodeKind::Interface => {
            encloses(facts, call.caller_file, call.caller_owner, owner) != Some(false)
        }
        _ => true,
    }
}

/// The method `call` binds in place of `target`: `target` when C# admits
/// it, else the one admitted overload among `later`, the later overloads of
/// `target`. C# skips an overload the caller cannot reach (a private first
/// `WriteTo(JsonWriter)` beside a public `WriteTo(JsonWriter, params ..)`).
/// `None` when no overload, or more than one, is admitted.
pub fn admitted_overload<F: PickFacts + ?Sized>(
    facts: &F,
    target: u32,
    later: &[u32],
    flags_of: impl Fn(u32) -> u16,
    call: CallAccess<'_>,
) -> Option<u32> {
    let admits = |id: u32| call_admits(facts, id, flags_of(id), call);
    if admits(target) {
        return Some(target);
    }
    let mut admitted = later.iter().copied().filter(|&id| admits(id));
    let first = admitted.next()?;
    admitted.next().is_none().then_some(first)
}

/// Whether a type named `target_owner` is the caller's type or encloses it.
/// Names compare, not ids, so a `partial` type declared across files
/// counts as one. `None` when the caller's owner is no type `caller_file`
/// declares.
fn encloses<F: PickFacts + ?Sized>(
    facts: &F,
    caller_file: &str,
    caller_owner: Option<&str>,
    target_owner: &str,
) -> Option<bool> {
    let link = caller_owner?;
    if link == target_owner {
        return Some(true);
    }
    let mut current = type_in(facts, caller_file, link)?;
    for _ in 0..NESTING_LIMIT {
        let Some(outer) = facts.owner(current) else {
            return Some(false);
        };
        // The outermost type's owner is its namespace.
        let Some(outer_type) = type_in(facts, caller_file, outer) else {
            return Some(false);
        };
        if outer == target_owner {
            return Some(true);
        }
        current = outer_type;
    }
    None
}

/// A type named `owner` that `file` declares.
fn type_in<F: PickFacts + ?Sized>(facts: &F, file: &str, owner: &str) -> Option<u32> {
    facts
        .types_named(owner)
        .into_iter()
        .find(|&id| facts.file_of(id) == Some(file))
}
