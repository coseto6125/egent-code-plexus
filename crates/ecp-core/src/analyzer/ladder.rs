//! The name tiers of the call-resolution ladder that the index
//! (`Resolver::resolve_symbol_with_import_binding`) and the session overlay
//! (`resolve_callee` in `session/view/resolution.rs`) both run: Tier 1 (the caller's
//! file), Tier 2.75 (the caller's bases) and Tier 3 (one name across the
//! project). The rules that pick live here once (B §2.2, row B5); each side
//! answers the questions through its own [`ResolveFacts`].
//!
//! Each side keeps what only it has: the decision dump (index), the Tier 3
//! memo (overlay), and a post-filter the overlay applies to every tier. The
//! file a base resolves to and the Tier 3 candidate stream read per-call
//! inputs (the caller's imports, the overlay's name tables) that neither
//! facts store holds, so they arrive as closures.
//!
//! A body's `field_reads` reach the same tiers through the entry points at
//! the end: a plain field read ([`field_read`], R24), a name reference
//! ([`name_ref`], R25) and an enum-variant reference ([`bare_variant`],
//! [`qualified_variant`], R26).
//!
//! The import-member tier of Python and the fully qualified languages
//! (R8: [`import_member`], [`imported_member`]), its wildcard pass after the
//! heritage tier (R19: [`wildcard_member`]) and the Kotlin extension retry of
//! the ladder (R17: [`kotlin_extension_retry`], [`extension_fallback`]) read
//! module discovery and the member a module file holds through
//! [`ImportModules`] and [`ImportMembers`]: the index's `SymbolTable` has no
//! module discovery, so these facts sit beside [`ResolveFacts`].
//!
//! A qualified callee (`K.save`, `K::st`, `pkg.Fn`) takes [`qualified`]: a
//! Python `super().m()` the caller's bases (R12, [`super_member`]), else the
//! member in the qualifier's file (R13, [`qualifier_file`],
//! [`qualified_member`]), else for a `::` path the Rust module tree (R14),
//! whose lookup each side keeps. Those read [`QualifierFacts`].
//!
//! One call site enters through [`call`] (R1): it normalizes the site (R2),
//! derives the call pick from it, runs the side's ladder, then applies the
//! call-level Kotlin extension retry (R17), the untyped-member post-filter
//! (R21) and the constructor fallback (R22, [`constructed_type`]) by the
//! B §3.2 table ([`Resolution::call_fallbacks_allowed`]). Each side answers
//! through its [`CallFacts`].

use crate::analyzer::import_binding::{
    extension_retry, fqn_language, import_binding, import_family, import_member_fallback,
    may_suppress, module_head, retry_name, MemberOwner,
};
use crate::analyzer::module_discovery::Module;
use crate::analyzer::name_ref::resolve_name_ref;
use crate::analyzer::outcome::{Hit, NoEdge, Resolution, Tier};
use crate::analyzer::reexport::{
    self, is_ecmascript, pick_call, GoPackageLookup, ImportRef, ModuleFacts, ResolveTarget,
};
use crate::analyzer::rust_paths::rust_path_prefix_agrees_with_import;
use crate::analyzer::same_file_pick::{
    is_free, member_call_admits, prefer_free, prefer_owned, type_receiver_is_closed, PickFacts,
    PickScope,
};
use crate::analyzer::type_ladder::{member_ownership, Ownership, TypeCandidates, TypeFacts};
use crate::analyzer::types::{owner_key, CallSite, NameRef, RawImport};
use crate::file_category::{pick_global, FileMeta, GlobalPick, Language};
use crate::graph::NodeKind;

/// What the name tiers read besides [`TypeFacts`]. Each method answers a
/// question about the declarations; the tiers below decide among them.
pub trait ResolveFacts: TypeFacts {
    /// `file`'s declarations of `name` in source order, with their kinds, as
    /// a name lookup sees them: no scoped declaration, no later overload.
    fn call_decls(&self, file: &str, name: &str) -> impl Iterator<Item = (u32, NodeKind)> + Clone;
    /// The first declaration of `name` in `file` of `target`'s kinds, in
    /// source order. A scoped one counts: [`prefer_owned`] puts the first
    /// unscoped one in its place.
    fn first_decl_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> Option<u32>;
    /// Whether the index's global name table holds a declaration named
    /// `name`, of any kind: the gate before a name reference reads a tier.
    fn has_named(&self, name: &str) -> bool;
    /// The `EnumVariant` named `variant` that the type `type_id` owns in the
    /// type's file. `type_name` is the name the reference used for the type;
    /// when an import alias (`use E as F`) makes it differ from the declared
    /// name, the declared name is tried too.
    fn enum_variant_of(&self, type_id: u32, type_name: &str, variant: &str) -> Option<u32>;
}

/// What a call site adds to a name lookup. The index's
/// `Resolver::resolve_call` and the overlay's edge pass both build one per
/// call site; `None` in its place is a plain symbol lookup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallPick<'c> {
    /// The owner key of the type the calling member belongs to.
    pub owner: Option<&'c str>,
    /// The calling declaration: a Rust `self.m()` binds its own `impl`
    /// block first ([`prefer_owned`]).
    pub caller: Option<u32>,
    /// `o.m()` on a receiver the parser could not type: no `this`, so the
    /// caller's bases do not narrow the pick and the heritage tier skips.
    pub untyped: bool,
    /// A bare `f()` that never reaches a member: Tier 1 weighs no `Method`
    /// and the heritage tier skips. Each side's Tier 3 candidate stream
    /// drops the methods itself.
    pub free_only: bool,
}

/// Tier 1: `name` among `file`'s own declarations of `target`'s kinds, the
/// first in source order ([`pick_call`]). A call site narrows the pick by
/// owner ([`prefer_owned`], with `heritage` unless the receiver is
/// untyped), or to a free function when it reaches no member
/// ([`prefer_free`]). `Unknown` when the file declares no such name, or
/// holds several unrelated owners of it: the later tiers decide.
pub fn same_file<F: ResolveFacts>(
    facts: &F,
    file: &str,
    name: &str,
    target: ResolveTarget,
    call: Option<CallPick<'_>>,
    heritage: &[String],
) -> Resolution {
    let free_only = call.is_some_and(|call| call.free_only);
    let decls = facts
        .call_decls(file, name)
        .filter(move |&(_, kind)| !free_only || kind != NodeKind::Method);
    let Some(pick) = pick_call(decls, target) else {
        return Resolution::Unknown;
    };
    let hit = match call {
        Some(call) if free_only => prefer_free(facts, file, name, call.owner, target),
        Some(call) => prefer_owned(
            facts,
            file,
            pick,
            name,
            call.owner,
            if call.untyped { &[] } else { heritage },
            target,
            PickScope {
                caller: call.caller,
                typed_receiver: false,
                untyped_receiver: call.untyped,
                qualifier_is_type: false,
            },
        ),
        None => Some(pick),
    };
    match hit.filter(|&id| !free_only || facts.kind(id) != NodeKind::Method) {
        Some(id) => Resolution::bound(id, Tier::SameFile),
        None => Resolution::Unknown,
    }
}

/// Tier 2.75: a bare `name` in a type with declared `bases`. Each base, in
/// declared order (the parser's source order, which mirrors MRO
/// precedence), is an implicit qualifier: `base_file` names the file that
/// declares it, and that file's member owned by the base binds. The first
/// base that binds decides, and its name comes back with the edge. Only
/// the listed bases are probed, each once, so a supertype cycle ends.
/// `Unknown` for an untyped receiver or a call that reaches no member.
pub fn heritage<'b, F: ResolveFacts, P: AsRef<str>>(
    facts: &F,
    bases: &'b [String],
    name: &str,
    target: ResolveTarget,
    call: Option<CallPick<'_>>,
    mut base_file: impl FnMut(&str) -> Option<P>,
) -> (Resolution, Option<&'b str>) {
    if call.is_some_and(|call| call.untyped || call.free_only) {
        return (Resolution::Unknown, None);
    }
    for base in bases {
        let Some(declaring) = base_file(base.as_str()) else {
            continue;
        };
        let file = declaring.as_ref();
        let hit = facts
            .first_decl_in_file(file, name, target)
            .and_then(|pick| {
                prefer_owned(
                    facts,
                    file,
                    pick,
                    name,
                    Some(base.as_str()),
                    &[],
                    target,
                    PickScope {
                        typed_receiver: true,
                        ..PickScope::default()
                    },
                )
            });
        if let Some(id) = hit {
            return (
                Resolution::bound(id, Tier::HeritageScoped),
                Some(base.as_str()),
            );
        }
    }
    (Resolution::Unknown, None)
}

/// The Tier 3 verdict over `candidates`, same-named declarations with
/// their files' metas: [`pick_global`]'s barriers over the unscoped ones
/// (I5), then a unique pick outside `target`'s kinds is `NoMatch`. Method
/// eligibility is this post-filter, not a candidate filter: dropping free
/// functions before the uniqueness check would invent a winner.
pub fn global_pick<F: PickFacts + ?Sized>(
    facts: &F,
    caller: FileMeta,
    target: ResolveTarget,
    candidates: impl IntoIterator<Item = (u32, FileMeta)>,
) -> GlobalPick {
    let unscoped = candidates
        .into_iter()
        .filter(|&(id, _)| !facts.is_lexically_scoped(id));
    match pick_global(caller, unscoped) {
        GlobalPick::Unique(id) if !target.kind_predicate()(facts.kind(id)) => GlobalPick::NoMatch,
        other => other,
    }
}

/// The copy nearest `caller_file` when every candidate `caller` admits is
/// a copy of the first ([`TypeFacts::is_copy`]): duplicated source trees
/// (`android/` beside the main tree, vendored copies) hold such copies, and
/// the build compiles the caller against its own tree's. The walk stops at
/// the first candidate that is no copy, so a common name with many
/// unrelated declarations costs two comparisons. `None` for a tie or for
/// fewer than two copies ([`TypeFacts::nearest_copy`]).
pub fn nearest_copy<F: TypeFacts + ?Sized>(
    facts: &F,
    caller_file: &str,
    caller: FileMeta,
    candidates: impl IntoIterator<Item = (u32, FileMeta)>,
) -> Option<u32> {
    let mut copies: Vec<u32> = Vec::new();
    for (id, meta) in candidates {
        if !caller.admits(meta) || facts.is_lexically_scoped(id) {
            continue;
        }
        if copies
            .first()
            .is_some_and(|&first| !facts.is_copy(first, id))
        {
            return None;
        }
        copies.push(id);
    }
    facts.nearest_copy(caller_file, &copies)
}

/// Tier 3: one `name` across the project. `candidates` yields the
/// same-named declarations of the field the call may reach, with their
/// files' metas; it is read again for the nearest copy and the count.
/// A unique pick ([`global_pick`]) binds unless `hidden` (a TS / JS import
/// naming the pick's private ES module) refuses it. Several candidates bind
/// nothing, an invented edge being worse than a missing one, unless
/// `nearest` (a call site) and all are copies of one declaration: then the
/// copy nearest the caller binds ([`nearest_copy`]). A refused or absent
/// copy leaves `NoEdge::Ambiguous` with the number of candidates that fit.
pub fn global<F: TypeFacts + ?Sized, I: Iterator<Item = (u32, FileMeta)>>(
    facts: &F,
    caller_file: &str,
    caller: FileMeta,
    target: ResolveTarget,
    nearest: bool,
    candidates: impl Fn() -> I,
    hidden: impl Fn(u32) -> bool,
) -> Resolution {
    match global_pick(facts, caller, target, candidates()) {
        GlobalPick::Unique(id) if hidden(id) => Resolution::NoEdge(NoEdge::Hidden),
        GlobalPick::Unique(id) => Resolution::bound(id, Tier::Global),
        GlobalPick::Ambiguous => {
            let copy = nearest
                .then(|| nearest_copy(facts, caller_file, caller, candidates()))
                .flatten()
                .filter(|&id| target.kind_predicate()(facts.kind(id)) && !hidden(id));
            match copy {
                Some(id) => Resolution::bound(id, Tier::NearestCopy),
                None => {
                    let fit = candidates()
                        .filter(|&(id, meta)| caller.admits(meta) && !facts.is_lexically_scoped(id))
                        .count();
                    Resolution::NoEdge(NoEdge::Ambiguous(u32::try_from(fit).unwrap_or(u32::MAX)))
                }
            }
        }
        GlobalPick::NoMatch => Resolution::NoEdge(NoEdge::Unresolved),
    }
}

/// Split a qualified name into `(qualifier, member)`, where `qualifier` is
/// the identifier right before the rightmost `::`, `.` or `\`. `None` for a
/// bare name or an empty side.
///
/// Only the last segment is the qualifier: it is the one piece a lookup can
/// map back to a registered type name (`std::vec::Vec` is registered as
/// `Vec`, keyed by its defining file). PHP's `\` counts beside `::` and `.`.
///
/// * `A::new` → `("A", "new")`
/// * `std::vec::Vec::new` → `("Vec", "new")`
/// * `obj.method` → `("obj", "method")`
/// * `\App\helper` → `("App", "helper")`
/// * `foo` → `None`
pub fn split_qualifier(name: &str) -> Option<(&str, &str)> {
    let (split_idx, sep_len) = [
        (name.rfind("::"), 2usize),
        (name.rfind('.'), 1),
        (name.rfind('\\'), 1),
    ]
    .into_iter()
    .filter_map(|(idx, len)| Some((idx?, len)))
    .max_by_key(|&(idx, _)| idx)?;
    let (before, after) = name.split_at(split_idx);
    let member = &after[sep_len..];
    if before.is_empty() || member.is_empty() {
        return None;
    }
    let qualifier = before
        .rsplit_once("::")
        .or_else(|| before.rsplit_once('.'))
        .or_else(|| before.rsplit_once('\\'))
        .map_or(before, |(_, q)| q);
    (!qualifier.is_empty()).then_some((qualifier, member))
}

/// Where a plain field read (`c.timeout`: no [`NameRef`] and no variant
/// tag) goes after [`field_read`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldRead {
    /// Tier 1 bound the read to a `Property` of the reader's file.
    Bound(Hit),
    /// A qualified read: the side's full name lookup of
    /// [`ResolveTarget::Field`] runs, and its qualifier tier settles it.
    Qualified,
    /// A bare read that Tier 1 left: Tier 3 ([`global`]) with no call site.
    Global,
}

/// R24: the tiers a plain field read reaches. A bare read takes Tier 1, then
/// Tier 3. A qualified read never reaches Tier 3: it resolves through its
/// qualifier or not at all. No import tier runs, because an import binds a
/// module's name, never the field a receiver reads. No call site: no owner
/// narrows the pick, no base is probed, and Tier 3 binds no nearest copy.
pub fn field_read<F: ResolveFacts>(facts: &F, file: &str, name: &str) -> FieldRead {
    if split_qualifier(name).is_some() {
        return FieldRead::Qualified;
    }
    match same_file(facts, file, name, ResolveTarget::Field, None, &[]) {
        Resolution::Bound(hit) => FieldRead::Bound(hit),
        _ => FieldRead::Global,
    }
}

/// R25: the declaration a [`NameRef`] names, with the edge confidence. A
/// function that a lexical scope in reach declares binds first, at Tier 1's
/// confidence: `lexical` is the side's `LexicalFunctions::name_ref_target`,
/// as the name tiers never return a scoped node (I5). Then the name tiers
/// ([`resolve_name_ref`]); `resolve` and `resolve_imported` are the side's
/// full name lookup and its import tiers alone.
#[allow(clippy::too_many_arguments)]
pub fn name_ref<F, Resolve, ResolveImported>(
    facts: &F,
    file: &str,
    language: Language,
    imports: &[RawImport],
    owner: Option<&str>,
    name_ref: NameRef<'_>,
    lexical: Option<u32>,
    resolve: Resolve,
    resolve_imported: ResolveImported,
) -> Option<(u32, f32)>
where
    F: ResolveFacts,
    Resolve: FnMut(&str, ResolveTarget) -> Vec<(u32, f32)>,
    ResolveImported: FnMut(&str, ResolveTarget) -> Vec<(u32, f32)>,
{
    if let Some(id) = lexical {
        return Some((id, Tier::SameFile.confidence()));
    }
    resolve_name_ref(
        facts,
        file,
        language,
        imports,
        owner,
        name_ref,
        resolve,
        resolve_imported,
    )
}

/// R26, a bare variant name (C `RED`, a Java `case MONDAY:`): the
/// `EnumVariant` it denotes. `has_variant` gates first: most captured names
/// name no variant. Then the reader's file decides when it declares the
/// name at all (`in_file`, every declaration of it, scoped ones as the
/// side's store holds them): only a sole variant binds (Tier 1), as a local
/// `#define` or constant of the name shadows the enumerator. Otherwise the
/// one value of the name the caller's barrier admits project-wide
/// ([`pick_global`] over `values`) binds when it is a variant (Tier 3).
pub fn bare_variant<F, D, V>(
    facts: &F,
    name: &str,
    caller: FileMeta,
    has_variant: impl Fn(&str) -> bool,
    in_file: impl FnOnce() -> Option<D>,
    values: impl FnOnce() -> V,
) -> Resolution
where
    F: PickFacts + ?Sized,
    D: IntoIterator<Item = (u32, NodeKind)>,
    V: IntoIterator<Item = (u32, FileMeta)>,
{
    if !has_variant(name) {
        return Resolution::NoEdge(NoEdge::Unresolved);
    }
    if let Some(decls) = in_file() {
        let mut decls = decls.into_iter();
        return match (decls.next(), decls.next()) {
            (Some((id, NodeKind::EnumVariant)), None) => Resolution::bound(id, Tier::SameFile),
            _ => Resolution::NoEdge(NoEdge::Unresolved),
        };
    }
    match pick_global(caller, values()) {
        GlobalPick::Unique(id) if facts.kind(id) == NodeKind::EnumVariant => {
            Resolution::bound(id, Tier::Global)
        }
        _ => Resolution::NoEdge(NoEdge::Unresolved),
    }
}

/// R26, a qualified variant path (`E::A`, `Color.RED`): `has_variant` gates
/// on the variant, the qualifier resolves as a type (`qualifier_type`, the
/// side's unique `ResolveTarget::Type` lookup), and that type's own variant
/// binds at the type's confidence, so `A` of two enums never mixes. Rust
/// `Self` names the reader's own impl type (`owner_class`).
pub fn qualified_variant<'r, F: ResolveFacts>(
    facts: &F,
    language: Language,
    owner_class: Option<&'r str>,
    (qualifier, variant): (&'r str, &'r str),
    has_variant: impl Fn(&str) -> bool,
    qualifier_type: impl FnOnce(&'r str) -> Option<(u32, f32)>,
) -> Option<(u32, f32)> {
    if !has_variant(variant) {
        return None;
    }
    let qualifier = match (language, qualifier) {
        (Language::Rust, "Self") => owner_class.and_then(owner_key)?,
        _ => qualifier,
    };
    let (type_id, confidence) = qualifier_type(qualifier)?;
    facts
        .enum_variant_of(type_id, qualifier, variant)
        .map(|id| (id, confidence))
}

/// What the import tiers read of module discovery. Each method answers a
/// question; [`import_is_external`] and [`import_member_hit`] decide.
pub trait ImportModules {
    /// A module file path [`Self::import_module`] hands back.
    type Module: AsRef<str>;
    /// The module `specifier` names from `importer` among the files of
    /// `language` ([`crate::analyzer::module_discovery::discover_module`],
    /// through the side's memo).
    fn import_module(
        &self,
        importer: &str,
        specifier: &str,
        language: Language,
    ) -> Module<Self::Module>;
    /// Whether a file stem or directory of `language` is named `name`.
    fn module_name_indexed(&self, language: Language, name: &str) -> bool;
}

/// What the import-member tier reads of a module file.
pub trait ImportMembers: ImportModules {
    /// The first declaration of `name` of `target`'s kinds in `file` whose
    /// owner satisfies `owner`: for [`MemberOwner::Any`], the match the
    /// same-file pick takes with no owner (a free function, else one type's
    /// member); for `TopLevel`, one with no owning type; for `Type`, a member
    /// of the type its path names, when `file`'s types agree with that path.
    fn member_in_file(
        &self,
        file: &str,
        name: &str,
        target: ResolveTarget,
        owner: MemberOwner<'_>,
    ) -> Option<u32>;
    /// The declaration the Python package `module` re-exports as `name`
    /// ([`crate::analyzer::reexport::python_import_reexport`]); `None` for an
    /// import of another language.
    fn python_reexport(
        &self,
        language: Language,
        module: &str,
        name: &str,
        target: ResolveTarget,
    ) -> Option<u32>;
}

/// Whether `import` proves its callee external: [`may_suppress`] allows it,
/// and no language of its [`import_family`] indexes a module name equal to
/// its head or holds its module. Missing discovery alone is not proof: a
/// wrong suppression drops a genuine caller.
pub fn import_is_external<F: ImportModules + ?Sized>(
    facts: &F,
    importer: &str,
    import: &RawImport,
    language: Language,
) -> bool {
    let head = module_head(&import.source);
    may_suppress(import, language)
        && import_family(language).all(|family| !facts.module_name_indexed(family, head))
        && import_family(language).all(|family| {
            matches!(
                facts.import_module(importer, &import.source, family),
                Module::Missing
            )
        })
}

/// The member an import binds, the import, and the Python package whose
/// re-export the member went through (`barrel`).
#[derive(Debug, Clone)]
pub struct ImportHit<'i, P> {
    pub id: u32,
    pub import: &'i RawImport,
    pub barrel: Option<P>,
}

/// Two hits are equal when they name one member through the same import
/// record, not an equal-looking one.
impl<P: PartialEq> PartialEq for ImportHit<'_, P> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && std::ptr::eq(self.import, other.import)
            && self.barrel == other.barrel
    }
}

/// One pass of the import-member tier ([`import_member_hit`]).
#[derive(Debug, Clone, PartialEq)]
pub enum MemberHit<'i, P> {
    Bound(ImportHit<'i, P>),
    /// No unique member. `bound` says whether an import of the pass binds
    /// the name at all.
    Unbound {
        bound: bool,
    },
}

/// R8, one pass: the member that the explicit imports binding `name` (or,
/// with `wildcard`, the wildcard imports) hold in their in-repo modules. A
/// Python package module that lacks the member may re-export it. Python
/// keeps the first hit in source order, as Python rebinds a name per import.
/// In a fully qualified language two imports that hold different members
/// are no hit: the language rejects the call or picks by signature, so the
/// general tiers decide.
pub fn import_member_hit<'i, F: ImportMembers + ?Sized>(
    facts: &F,
    importer: &str,
    language: Language,
    name: &'i str,
    imports: &'i [RawImport],
    target: ResolveTarget,
    wildcard: bool,
) -> MemberHit<'i, F::Module> {
    let mut found: Option<(u32, &'i RawImport)> = None;
    let mut bound = false;
    for import in imports {
        let Some(binding) =
            import_binding(import, name, language).filter(|binding| binding.wildcard == wildcard)
        else {
            continue;
        };
        bound = true;
        let in_file = |file: &str| facts.member_in_file(file, binding.name, target, binding.owner);
        let hit = match facts.import_module(importer, &import.source, language) {
            Module::Unique(module) => match in_file(module.as_ref()) {
                Some(id) => Some((id, None)),
                None => facts
                    .python_reexport(language, module.as_ref(), binding.name, target)
                    .map(|id| (id, Some(module))),
            },
            Module::Namespace(files) => {
                let mut hits = files.iter().filter_map(|file| in_file(file.as_ref()));
                hits.next()
                    .filter(|_| hits.next().is_none())
                    .map(|id| (id, None))
            }
            Module::Missing | Module::Ambiguous => None,
        };
        let Some((id, barrel)) = hit else {
            continue;
        };
        if !fqn_language(language) {
            return MemberHit::Bound(ImportHit { id, import, barrel });
        }
        match found {
            Some((previous, _)) if previous != id => return MemberHit::Unbound { bound },
            Some(_) => {}
            None => found = Some((id, import)),
        }
    }
    found.map_or(MemberHit::Unbound { bound }, |(id, import)| {
        MemberHit::Bound(ImportHit {
            id,
            import,
            barrel: None,
        })
    })
}

/// How the import-member tier ([`import_member`]) settles one name.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportMember<'i, P> {
    /// An explicit import's module holds the member: Tier 2 binds it.
    Bound(ImportHit<'i, P>),
    /// Every import binding the name proves it external: no edge, and no
    /// later tier runs.
    External,
    /// A Kotlin call through an external class binding: the side resolves
    /// this bare member outside the call scope ([`extension_fallback`]).
    Extension(&'i str),
    /// Python: the side resolves this name through its remaining tiers,
    /// without imports.
    Retry(&'i str),
    /// The remaining tiers run on the name with its imports. `wildcard`:
    /// the wildcard imports bind after the heritage tier
    /// ([`wildcard_member`]).
    Continue { wildcard: bool },
}

/// R8, Tier 2 for a language whose calls bind through an import's member
/// (`import_member_fallback`); the caller gates on that, on non-empty
/// `imports` and on a target other than `Field`. An explicit import binding
/// `name` whose module holds the member decides the target. When none does,
/// a binding whose module may be local (relative, single segment, first
/// segment an indexed module name, or module found) keeps the general
/// tiers, under the callee as written, or the member's short name for a
/// Python star import ([`retry_name`]). Only when every binding import is
/// certainly external ([`import_is_external`]) is there no edge, unless a
/// Kotlin class binding retries its bare member as an extension function
/// ([`extension_retry`]). `super.x()` (a qualifier among the caller's
/// `heritage`) names the caller's base, which no extension call does. A
/// wildcard binds only when no explicit import binds the name, and below
/// the caller's heritage.
pub fn import_member<'i, F: ImportMembers + ?Sized>(
    facts: &F,
    importer: &str,
    language: Language,
    name: &'i str,
    imports: &'i [RawImport],
    target: ResolveTarget,
    heritage: &[String],
) -> ImportMember<'i, F::Module> {
    let explicit_bound =
        match import_member_hit(facts, importer, language, name, imports, target, false) {
            MemberHit::Bound(hit) => return ImportMember::Bound(hit),
            MemberHit::Unbound { bound } => bound,
        };
    let mut retry = None;
    let mut extension = None;
    let mut bound = false;
    let mut external = true;
    for import in imports {
        let Some(binding) = import_binding(import, name, language) else {
            continue;
        };
        bound = true;
        retry = retry_name(import, binding.name, name, language);
        extension = extension.or(extension_retry(&binding, language));
        external = external && import_is_external(facts, importer, import, language);
    }
    // No import binds the name: the wildcard pass would find no binding
    // either.
    if !bound {
        return ImportMember::Continue { wildcard: false };
    }
    if external {
        let heritage_call = split_qualifier(name)
            .is_some_and(|(qualifier, _)| heritage.iter().any(|base| base == qualifier));
        return match extension.filter(|_| !heritage_call) {
            Some(member) => ImportMember::Extension(member),
            None => ImportMember::External,
        };
    }
    match retry {
        Some(name) => ImportMember::Retry(name),
        None => ImportMember::Continue {
            wildcard: !explicit_bound && fqn_language(language),
        },
    }
}

/// R8 on an import-only path (an imported type, a name a lexical scope
/// hides, a field a name reference reads): the explicit imports, then, with
/// `wildcards` and when no explicit import binds `name`, the wildcard ones.
/// No retry, extension or suppression: a miss is no edge.
pub fn imported_member<'i, F: ImportMembers + ?Sized>(
    facts: &F,
    importer: &str,
    language: Language,
    name: &'i str,
    imports: &'i [RawImport],
    target: ResolveTarget,
    wildcards: bool,
) -> MemberHit<'i, F::Module> {
    match import_member_hit(facts, importer, language, name, imports, target, false) {
        MemberHit::Unbound { bound: false } if wildcards => {
            import_member_hit(facts, importer, language, name, imports, target, true)
        }
        explicit => explicit,
    }
}

/// R19: the wildcard imports' pass, which runs after the heritage tier
/// (members in scope beat imports) when [`import_member`] left it `open`.
pub fn wildcard_member<'i, F: ImportMembers + ?Sized>(
    facts: &F,
    open: bool,
    importer: &str,
    language: Language,
    name: &'i str,
    imports: &'i [RawImport],
    target: ResolveTarget,
) -> Option<ImportHit<'i, F::Module>> {
    if !open {
        return None;
    }
    match import_member_hit(facts, importer, language, name, imports, target, true) {
        MemberHit::Bound(hit) => Some(hit),
        MemberHit::Unbound { .. } => None,
    }
}

/// R17, the ladder's part: the bare member a Kotlin `Type.member` retries
/// when the receiver-typing ladder bound nothing. A receiver named by a
/// class import reaches the ladder as `Type.member` only since the import
/// tier; before it, the call was the bare member, which finds an extension
/// function declared on the type in another file. A heritage qualifier
/// (`super.x()`) was always qualified, so it keeps the ladder's verdict.
pub fn kotlin_extension_retry<'n>(
    language: Language,
    name: &str,
    (qualifier, member): (&str, &'n str),
    imports: &[RawImport],
    heritage: &[String],
) -> Option<&'n str> {
    (language == Language::Kotlin
        && !heritage.iter().any(|base| base == qualifier)
        && imports.iter().any(|import| {
            import_binding(import, name, language)
                .and_then(|binding| extension_retry(&binding, language))
                .is_some()
        }))
    .then_some(member)
}

/// R17: what an extension retry of `member` keeps of `resolve`, the side's
/// name tiers for the bare member outside a call scope, with the caller's
/// heritage. Only a top-level function ([`is_free`]) can be an extension on
/// the type: a typed receiver of a library type (`gson.fromJson()`) or
/// `super.onResume()` must not land on a same-named member. Extension
/// receivers are not indexed, so the retry stays a by-name heuristic and
/// cannot verify the declared receiver type.
pub fn extension_fallback<'f, F, I>(
    facts: &'f F,
    member: &str,
    resolve: impl FnOnce(&str) -> I,
) -> impl Iterator<Item = (u32, f32)> + 'f
where
    F: PickFacts + ?Sized,
    I: IntoIterator<Item = (u32, f32)>,
    I::IntoIter: 'f,
{
    resolve(member)
        .into_iter()
        .filter(move |&(id, _)| is_free(facts, id))
}

/// The path before `qualifier` in `full_callee` is empty or a `crate` /
/// `self` / `super` chain, so the qualifier may name a project module:
/// `std::fs::read` → preceding `std` → `false`; `auto_ensure::ensure_fresh`
/// → `true`; `crate::auto_ensure::ensure_fresh` → `true`.
pub fn qualifier_prefix_is_internal(full_callee: &str, qualifier: &str) -> bool {
    let Some(member_split) = full_callee.rfind("::").or_else(|| full_callee.rfind('.')) else {
        return false;
    };
    let before_member = &full_callee[..member_split];
    let preceding = before_member
        .rsplit_once("::")
        .or_else(|| before_member.rsplit_once('.'))
        .map_or("", |(p, q)| if q == qualifier { p } else { "" });
    preceding.is_empty()
        || preceding
            .split("::")
            .all(|s| matches!(s, "crate" | "self" | "super"))
}

/// The Rust module a `use` head names when it is a workspace crate's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrateModule<P> {
    /// The head names no workspace crate: the import's candidate files
    /// decide.
    Local,
    /// A workspace crate's module. The file holds `member` under its own
    /// name (a renamed `pub use` re-export would land on an unrelated
    /// namesake); `None` when it does not, or when the side cannot place
    /// the module, so no later step stands in for the answer.
    Placed(Option<P>),
}

/// What the qualifier tier (R13), the Rust module-tree position (R14) and
/// the Python `super()` rule (R12) read besides [`ResolveFacts`]. Each
/// method answers a question about one file or one name; [`qualifier_file`],
/// [`qualified_member`] and [`qualified`] decide. A file comes back as an
/// [`ImportModules::Module`].
pub trait QualifierFacts: ImportModules {
    /// The declarations the tiers pick from.
    type Decls: ResolveFacts;
    fn decls(&self) -> &Self::Decls;
    /// Whether the caller's file `source` declares `name` of a
    /// [`ResolveTarget::Qualifier`] kind: a type, a namespace or a module.
    fn caller_declares(&self, source: &str, name: &str) -> bool;
    /// `path` as the side stores it. `None` for a file the side does not
    /// hold.
    fn stored_file(&self, path: &str) -> Option<Self::Module>;
    /// [`pick_global`] over the declarations of `qualifier` of a qualifier
    /// kind that the `caller` barrier weighs.
    fn global_qualifier(&self, qualifier: &str, caller: FileMeta) -> GlobalPick;
    /// Each candidate file of `specifier` from `importer`, in probe order,
    /// until `visit` returns `false`.
    fn for_each_candidate(&self, importer: &str, specifier: &str, visit: impl FnMut(&str) -> bool);
    /// Whether the module `file` exports `name` of `target`'s kinds
    /// ([`crate::analyzer::reexport::lookup_export_in_file`]).
    fn exports(&self, file: &str, name: &str, target: ResolveTarget) -> bool;
    /// Whether `file` holds the qualifier an import of `imported` binds
    /// ([`crate::analyzer::reexport::imported_in_file`]).
    fn holds_import(&self, file: &str, imported: &str, default_import: bool) -> bool;
    /// The file of the `crate` / `self` / `super`-anchored Rust module
    /// `module_path` as seen from `source`.
    fn rust_module_file(&self, source: &str, module_path: &str) -> Option<Self::Module>;
    /// The workspace crate module the Rust `use` path `module_path` names,
    /// with `member` of `target`'s kinds in it.
    fn crate_module(
        &self,
        source: &str,
        module_path: &str,
        member: &str,
        target: ResolveTarget,
    ) -> CrateModule<Self::Module>;
    /// May `qualifier` of the Rust path call `callee` (`a::b::Q::m`) be the
    /// type `candidate` declares? `None` where the side cannot tell: no
    /// file binds then.
    fn rust_path_agrees(
        &self,
        source: &str,
        callee: &str,
        qualifier: &str,
        candidate: &str,
        imports: &[RawImport],
    ) -> Option<bool>;
    /// The files whose stem is `stem`.
    fn files_by_stem(&self, stem: &str) -> impl Iterator<Item = &str>;
    /// The project types `name` can denote in `source` (step 1 of the
    /// receiver-typing ladder).
    fn type_candidates<'q>(
        &self,
        source: &str,
        name: &'q str,
        imports: &'q [RawImport],
    ) -> TypeCandidates<'q>;
}

/// Which file the last step of [`qualifier_file`], a module named by its
/// file, hands back. The two sides differ here; the parameter keeps each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemFile {
    /// The one file named after the qualifier in the caller's crate root,
    /// whether or not it declares the member: the index. A member it lacks
    /// leaves [`qualified_member`] no edge, and `this.m()` does not fall
    /// back to the caller's own file.
    Any,
    /// That file only when it declares the member: the overlay. A miss lets
    /// `this.m()` fall back to the caller's own file.
    DeclaringMember,
}

/// R13, the file part: the file that defines `qualifier` and declares
/// `member` of `target`'s kinds, by four steps in order. The caller's own
/// file; an import binding the qualifier (a module namespace import, an ES
/// `import * as ns`, a Rust module or workspace crate `use`, then the
/// import's candidate files); a unique definition project-wide; then a file
/// named after the qualifier in the caller's crate root (`mod util;` ↔
/// `util.rs`), never for an external path (`std::fs::read`).
///
/// `callee` is the full qualified callee. The heritage tier passes `None`,
/// which skips the Rust path checks and the file-name step: a base class
/// resolves as a type, never as a module file.
pub fn qualifier_file<F: QualifierFacts>(
    facts: &F,
    source: &str,
    caller: FileMeta,
    imports: &[RawImport],
    (callee, qualifier, member): (Option<&str>, &str, &str),
    target: ResolveTarget,
    stem: StemFile,
) -> Option<F::Module> {
    let decls = facts.decls();
    let language = caller.language;
    let declares_member = |file: &str| decls.first_decl_in_file(file, member, target).is_some();
    let agrees = |file: &str| {
        callee.map_or(Some(true), |callee| {
            facts.rust_path_agrees(source, callee, qualifier, file, imports)
        })
    };
    // The caller's own file, only when it holds the member too: a
    // file-backed `mod m;` declares the module here and its members in
    // `m.rs`, which the file-name step finds.
    if facts.caller_declares(source, qualifier) && declares_member(source) {
        match agrees(source) {
            Some(true) => return facts.stored_file(source),
            Some(false) => {}
            None => return None,
        }
    }

    for import in imports
        .iter()
        .filter(|i| i.alias.as_deref().unwrap_or(&i.imported_name) == qualifier)
    {
        if callee.is_some_and(|callee| {
            !rust_path_prefix_agrees_with_import(callee, qualifier, &import.source, imports)
        }) {
            continue;
        }
        let exported = import.imported_name.as_str();
        // A module namespace import: its module is the scope. A fully
        // qualified `*` import (a wildcard, a PHP group member) names no
        // module the qualifier stands for, so it keeps the candidate probe.
        if exported == "*" && import_member_fallback(language) && !fqn_language(language) {
            match facts.import_module(source, &import.source, language) {
                Module::Unique(file) => return Some(file),
                _ => continue,
            }
        }
        // `import * as ns from './a'`: the module is the scope, and a miss
        // ends the search: `ns.f()` never means another file's `f`.
        if exported == "*" && is_ecmascript(language) {
            let mut hit = None;
            facts.for_each_candidate(source, &import.source, |candidate| {
                let found = facts.exports(candidate, member, target);
                if found {
                    hit = facts.stored_file(candidate);
                }
                !found
            });
            return hit;
        }
        if language == Language::Rust {
            // `use a::m; m::f()`: `m` may be the module `a::m`, whose own
            // file then decides. The parent file declaring `mod m;` may
            // define an `f` of its own, so it is not consulted.
            let module_path = if exported.contains("::") {
                exported.to_owned()
            } else {
                format!("{}::{exported}", import.source)
            };
            if let Some(module_file) = facts.rust_module_file(source, &module_path) {
                return declares_member(module_file.as_ref()).then_some(module_file);
            }
            // A head that is a module of the caller's own file (`mod utils;
            // use utils::fs;`) is no crate path.
            let head = module_path.split("::").next().unwrap_or_default();
            if !facts.caller_declares(source, head) {
                if let CrateModule::Placed(file) =
                    facts.crate_module(source, &module_path, member, target)
                {
                    return file;
                }
            }
        }
        // Through the module's exports: an ES module may export the
        // qualifier under another name (`export { Impl as Widget }`).
        let default_import = ImportRef::from(import).is_es_default();
        let mut hit = None;
        facts.for_each_candidate(source, &import.source, |candidate| {
            let found = facts.holds_import(candidate, exported, default_import)
                && declares_member(candidate);
            if found {
                hit = facts.stored_file(candidate);
            }
            !found
        });
        if hit.is_some() {
            return hit;
        }
    }

    if let GlobalPick::Unique(id) = facts.global_qualifier(qualifier, caller) {
        if let Some(file) = decls.file_of(id).filter(|&file| declares_member(file)) {
            match agrees(file) {
                Some(true) => return facts.stored_file(file),
                Some(false) => {}
                None => return None,
            }
        }
    }

    // A module named by its file (`mod util;` ↔ `util.rs`, `import foo` ↔
    // `foo.py`), unique within the caller's crate root (`*/src/`,
    // `*/tests/`). An external path (`std::fs::read`) never: its preceding
    // segment is no `crate` / `self` / `super`.
    let callee = callee?;
    if !qualifier_prefix_is_internal(callee, qualifier) {
        return None;
    }
    let root = decls.crate_root(source);
    let mut named = facts
        .files_by_stem(qualifier)
        .filter(|&file| decls.crate_root(file) == root);
    let file = match (named.next(), named.next()) {
        (Some(file), None) => file,
        _ => return None,
    };
    match stem {
        StemFile::DeclaringMember if !declares_member(file) => None,
        _ => facts.stored_file(file),
    }
}

/// One qualified call site: `callee` as written, split into `qualifier` and
/// `member` ([`split_qualifier`]), in the file `source` of `caller`'s meta.
/// `language` is the language the side resolves the call in.
#[derive(Clone, Copy)]
pub struct QualifiedSite<'s> {
    pub source: &'s str,
    pub caller: FileMeta,
    pub language: Language,
    pub imports: &'s [RawImport],
    pub callee: &'s str,
    pub qualifier: &'s str,
    pub member: &'s str,
    pub target: ResolveTarget,
    pub call: Option<CallPick<'s>>,
}

/// R13, the member part: in the file [`qualifier_file`] picks, the `member`
/// that [`prefer_owned`] picks with the qualifier as the receiver's type.
/// `this.m()` typed as the caller's own type (`C.m`) takes the caller's
/// `heritage`, and a self type no file declares (Rust `impl W for Pin<P>`)
/// resolves in the caller's file. A module or namespace qualifier
/// (`mod.f()`) may reach a free function; a type reaches only its lineage.
///
/// A Rust path (`crate::x::parse`) binds a method only when the qualifier
/// is its owning type (`K::st`): a module path never names a method
/// ([`rust_path_may_bind`]).
pub fn qualified_member<F: QualifierFacts>(
    facts: &F,
    site: QualifiedSite<'_>,
    heritage: &[String],
    stem: StemFile,
) -> Option<u32> {
    let QualifiedSite {
        source,
        caller,
        language,
        imports,
        callee,
        qualifier,
        member,
        target,
        call,
    } = site;
    let decls = facts.decls();
    let caller_type = call
        .and_then(|call| call.owner)
        .and_then(owner_key)
        .is_some_and(|owner| owner_key(qualifier) == Some(owner));
    let file = qualifier_file(
        facts,
        source,
        caller,
        imports,
        (Some(callee), qualifier, member),
        target,
        stem,
    )
    .or_else(|| caller_type.then(|| facts.stored_file(source)).flatten())?;
    let file = file.as_ref();
    let heritage: &[String] = if caller_type { heritage } else { &[] };
    let pick = decls.first_decl_in_file(file, member, target)?;
    let qualifier_is_type = type_receiver_is_closed(language)
        && (caller_type
            || owner_key(qualifier).is_some_and(|key| {
                decls
                    .first_decl_in_file(file, key, ResolveTarget::Type)
                    .is_some()
            }));
    let id = prefer_owned(
        decls,
        file,
        pick,
        member,
        Some(qualifier),
        heritage,
        target,
        PickScope {
            caller: call.and_then(|call| call.caller),
            typed_receiver: true,
            untyped_receiver: false,
            qualifier_is_type,
        },
    )?;
    rust_path_may_bind(decls, site, id).then_some(id)
}

/// A Rust path (`crate::x::parse`) names a method only through its owning
/// type (`K::st`): a module path never names one, whichever tier found it.
fn rust_path_may_bind<D: PickFacts>(decls: &D, site: QualifiedSite<'_>, id: u32) -> bool {
    site.language != Language::Rust
        || !site.callee.contains("::")
        || decls.kind(id) != NodeKind::Method
        || decls.owner(id) == Some(site.qualifier)
}

/// R12, R13 and R14's position: the tiers a qualified callee takes, in
/// order. A Python `super().m()` takes [`super_member`] and nothing after.
/// Otherwise the member in the qualifier's file ([`qualified_member`]),
/// then for a `::` path the Rust module tree: `module_tree` is the side's
/// lookup of `site.callee` there, under the same method rule
/// ([`rust_path_may_bind`]). `Unknown` leaves the call to the side's
/// receiver-typing ladder: it runs only where these tiers bound nothing, so
/// it adds edges and never moves one. The base name comes back with a
/// `super()` edge.
///
/// No tier retries the bare member: a qualified callee resolves through its
/// qualifier or not at all. `std::fs::read` stripped to `read` would bind a
/// project `read`, and `serde_json::json!` a local `json()`.
pub fn qualified<'h, F: QualifierFacts>(
    facts: &F,
    site: QualifiedSite<'_>,
    heritage: &'h [String],
    stem: StemFile,
    module_tree: impl FnOnce() -> Option<u32>,
) -> (Resolution, Option<&'h str>) {
    if site.qualifier == CallSite::SUPER_RECEIVER && site.caller.language == Language::Python {
        return super_member(facts, site, heritage);
    }
    if let Some(id) = qualified_member(facts, site, heritage, stem) {
        return (Resolution::bound(id, Tier::QualifierScoped), None);
    }
    if site.callee.contains("::") {
        if let Some(id) = module_tree().filter(|&id| rust_path_may_bind(facts.decls(), site, id)) {
            return (Resolution::bound(id, Tier::ModuleTree), None);
        }
    }
    (Resolution::Unknown, None)
}

/// The most `pub use` hops [`module_tree_item`] follows from one item.
const MAX_REEXPORT_HOPS: u8 = 16;

/// One `pub use` re-export, as written in its module (R14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReExport<'f> {
    /// The path before the item: `inner` in `pub use inner::W`. It may start
    /// with `crate`, `self`, `super` or a workspace crate name; any other
    /// head is a child of the containing module.
    pub prefix: &'f [String],
    /// The item at the source (`W` in `pub use inner::W as V`), `*` for a
    /// glob.
    pub original: &'f str,
    /// The module path, in its tree, of the module holding the `pub use`.
    pub containing: &'f [String],
}

/// What a module publishes a name through, as one side knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published<'f> {
    /// No `pub use` publishes the name: the module's own declaration does,
    /// if it has one.
    Own,
    /// The `pub use` naming it, else a glob `pub use`.
    ReExport(ReExport<'f>),
    /// A `use` of the module may publish the name, and the side cannot see
    /// whether it is `pub`.
    Unknown,
}

/// Where a Rust path lands in the module tree ([`module_tree_item`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeItem<P> {
    /// The item `item` of the module whose file is `file`, past every
    /// `pub use` the side follows.
    Found { file: P, item: String },
    /// A `use` whose visibility the side cannot see stands on the path: no
    /// file binds.
    Unknown,
}

/// What the Rust module-tree tier (R14) reads: the module trees of the
/// workspace's Cargo targets. Each method answers a question about one
/// tree, one module or one caller; [`module_tree_item`] and
/// [`module_tree_member`] decide.
pub trait ModuleTreeFacts {
    /// One Cargo target's module tree: a crate's lib target or a bin.
    type Root: Copy;
    /// One module of a tree.
    type Module: Copy + PartialEq;
    /// A module's file as the side stores it.
    type File: AsRef<str>;
    /// One Cargo target, named for [`ModuleTreeFacts::declaring_targets`]
    /// and [`ModuleTreeFacts::target_module`].
    type Target: Copy;
    /// The lib target of the workspace crate that code names `name`, from
    /// the crate table. `None` for a name no crate carries, and for a name
    /// the table cannot give one crate for.
    fn crate_root(&self, name: &str) -> Option<Self::Root>;
    /// The target whose `crate::` the file `caller` writes.
    fn caller_crate(&self, caller: &str) -> Option<Self::Root>;
    /// The target and the module path of `caller`'s own module, which
    /// `self::` and `super::` start from. `None` when no target declares
    /// the caller.
    fn caller_module(&self, caller: &str) -> Option<(Self::Root, Vec<String>)>;
    /// The root module of `root`, its entry file.
    fn root_module(&self, root: Self::Root) -> Option<Self::Module>;
    /// The module at `path` (not empty) in `root`'s tree: a file-backed
    /// `mod` declaration of `parent`, the module at `path` less its last
    /// segment.
    fn submodule(
        &self,
        root: Self::Root,
        parent: Self::Module,
        path: &[String],
    ) -> Option<Self::Module>;
    /// What `module` of `root` publishes `name` through.
    fn published(&self, root: Self::Root, module: Self::Module, name: &str) -> Published<'_>;
    /// The stored file of `module`. `None` for a file the side cannot name.
    fn module_file(&self, module: Self::Module) -> Option<Self::File>;
    /// Each Cargo target (the lib, each bin) that declares the file
    /// `caller`, with the caller's module path in it. Empty when none does.
    fn declaring_targets(&self, caller: &str) -> Vec<(Self::Target, Vec<String>)>;
    /// The file of the module at `path` in `target`'s tree; `path` empty is
    /// the target's root file. `None` when the tree holds no such module.
    fn target_module(&self, target: Self::Target, path: &[String]) -> Option<Self::File>;
}

/// Heads that name a crate outside the workspace.
fn is_external_crate(head: &str) -> bool {
    matches!(head, "std" | "core" | "alloc" | "proc_macro" | "test")
}

/// The target and the crate-rooted module path of the head of `segs` from
/// `caller`, with the segments after that head: `crate`, `self`, a
/// `super` chain, or a workspace crate name.
fn tree_anchor<F: ModuleTreeFacts>(
    facts: &F,
    caller: &str,
    segs: &[&str],
) -> Option<(F::Root, Vec<String>)> {
    let owned = |segs: &[&str]| segs.iter().map(|&s| s.to_owned()).collect::<Vec<_>>();
    match *segs.first()? {
        "crate" => Some((facts.caller_crate(caller)?, owned(&segs[1..]))),
        head @ ("self" | "super") => {
            let (root, mut path) = facts.caller_module(caller)?;
            let up = segs.iter().take_while(|&&s| s == "super").count();
            path.truncate(path.len().checked_sub(up)?);
            let rest = if head == "self" {
                &segs[1..]
            } else {
                &segs[up..]
            };
            path.extend(rest.iter().map(|&s| s.to_owned()));
            Some((root, path))
        }
        head if is_external_crate(head) => None,
        head => Some((facts.crate_root(head)?, owned(&segs[1..]))),
    }
}

/// The deepest module of `root` that a prefix of `combined` (at most all
/// segments but the last) names, and the index of the segment after it.
/// A tree holds a module only below its parent, so the deepest prefix is
/// the one a walk from the root reaches.
fn tree_walk<F: ModuleTreeFacts>(
    facts: &F,
    root: F::Root,
    combined: &[String],
) -> Option<(F::Module, usize)> {
    if combined.is_empty() {
        return None;
    }
    let mut module = facts.root_module(root)?;
    let mut at = 0;
    while at + 1 < combined.len() {
        match facts.submodule(root, module, &combined[..=at]) {
            Some(child) => {
                module = child;
                at += 1;
            }
            None => break,
        }
    }
    Some((module, at))
}

/// The `pub use` chain from `item` of `module`. `None` when the module
/// publishes `item` itself, or when the chain leads back to it, past the
/// hop limit, or to a file the side cannot name: the caller keeps
/// `(module, item)` then.
fn tree_follow<F: ModuleTreeFacts>(
    facts: &F,
    root: F::Root,
    module: F::Module,
    item: &str,
    depth: u8,
) -> Option<TreeItem<F::File>> {
    if depth >= MAX_REEXPORT_HOPS || item.is_empty() {
        return None;
    }
    let entry = match facts.published(root, module, item) {
        Published::Own => return None,
        Published::Unknown => return Some(TreeItem::Unknown),
        Published::ReExport(entry) => entry,
    };
    let target = if entry.original == "*" {
        item
    } else {
        entry.original
    };
    let (prefix, none): (&[String], &[String]) = (entry.prefix, &[]);
    let (root, base, rest) = match prefix.first().map(String::as_str) {
        Some("crate") => (root, none, &prefix[1..]),
        Some("self") => (root, entry.containing, &prefix[1..]),
        Some("super") => {
            let up = prefix.iter().take_while(|s| s.as_str() == "super").count();
            let kept = entry.containing.len().saturating_sub(up);
            (root, &entry.containing[..kept], &prefix[up..])
        }
        Some(head) => match facts.crate_root(head) {
            Some(other) => (other, none, &prefix[1..]),
            None => (root, entry.containing, prefix),
        },
        None => (root, none, none),
    };
    let combined: Vec<String> = base
        .iter()
        .chain(rest)
        .cloned()
        .chain(std::iter::once(target.to_owned()))
        .collect();
    let (next, at) = tree_walk(facts, root, &combined)?;
    let next_item = &combined[at];
    if next == module && next_item == item {
        return None;
    }
    if let Some(deeper) = tree_follow(facts, root, next, next_item, depth + 1) {
        return Some(deeper);
    }
    Some(TreeItem::Found {
        file: facts.module_file(next)?,
        item: next_item.clone(),
    })
}

/// R14, the file part: where the Rust path `path` (`a::b::f`) lands in the
/// module tree, read from `caller`. The head picks the tree: `crate`, `self`
/// and `super` the caller's target, a workspace crate name that crate's lib
/// target; `std` and the other external crates none. The deepest module a
/// prefix of the path names holds the next segment as its item, and the
/// `pub use` chain from that item, at most 16 hops, gives the defining
/// module. `None` for a path of one segment, an unknown head, or a module
/// file the side cannot name.
pub fn module_tree_item<F: ModuleTreeFacts>(
    facts: &F,
    caller: &str,
    path: &str,
) -> Option<TreeItem<F::File>> {
    let segs: Vec<&str> = path.split("::").collect();
    if segs.len() < 2 {
        return None;
    }
    let (root, combined) = tree_anchor(facts, caller, &segs)?;
    let (module, at) = tree_walk(facts, root, &combined)?;
    let item = &combined[at];
    if let Some(found) = tree_follow(facts, root, module, item, 0) {
        return Some(found);
    }
    Some(TreeItem::Found {
        file: facts.module_file(module)?,
        item: item.clone(),
    })
}

/// The module that every segment of the module path `path` names, read
/// from `caller`: `b::util`, `crate::a`, `super`. `None` when a segment
/// names no module.
pub fn module_tree_module<F: ModuleTreeFacts>(
    facts: &F,
    caller: &str,
    path: &str,
) -> Option<F::Module> {
    let segs: Vec<&str> = path.split("::").collect();
    let (root, combined) = tree_anchor(facts, caller, &segs)?;
    let mut module = facts.root_module(root)?;
    for at in 0..combined.len() {
        module = facts.submodule(root, module, &combined[..=at])?;
    }
    Some(module)
}

/// The module path that the `crate` / `self` / `super`-anchored `segs` name
/// from the module at `mod_path`; `None` for another head, or a `super`
/// chain that climbs above the crate root.
fn anchored_mod_path(mod_path: &[String], segs: &[&str]) -> Option<Vec<String>> {
    let (base, rest) = match *segs.first()? {
        "crate" => (&mod_path[..0], &segs[1..]),
        "self" => (mod_path, &segs[1..]),
        "super" => {
            let up = segs.iter().take_while(|&&s| s == "super").count();
            (&mod_path[..mod_path.len().checked_sub(up)?], &segs[up..])
        }
        _ => return None,
    };
    Some(
        base.iter()
            .cloned()
            .chain(rest.iter().map(|s| s.to_string()))
            .collect(),
    )
}

/// The file of the module that the `crate` / `self` / `super`-anchored
/// `module_path` (`super::util`) names from `caller`, read from the tree of
/// each target that declares the caller: `crate` in a module of
/// `src/main.rs` is `main.rs`, and `super` in a `#[path]` file is the module
/// that declares it. `None` when no target declares the caller, when one of
/// them does not place the module, or when they place it at different files
/// (a module both the lib and a bin declare). The caller then falls back to
/// the file layout, as the index does.
pub fn anchored_module_file<F: ModuleTreeFacts>(
    facts: &F,
    caller: &str,
    module_path: &str,
) -> Option<F::File> {
    let segs: Vec<&str> = module_path.split("::").collect();
    let mut found: Option<F::File> = None;
    for (target, mod_path) in facts.declaring_targets(caller) {
        let key = anchored_mod_path(&mod_path, &segs)?;
        let file = facts.target_module(target, &key)?;
        if found.as_ref().is_some_and(|f| f.as_ref() != file.as_ref()) {
            return None;
        }
        found = Some(file);
    }
    found
}

/// R14, the member part: the declaration the Rust path `callee` binds
/// through the module tree. In the file [`module_tree_item`] places, the
/// item's first declaration of `target`'s kinds, else the first one named
/// `member` (the callee's last segment, generic arguments dropped).
/// [`qualified`] then applies the method rule ([`rust_path_may_bind`]).
pub fn module_tree_member<F: ModuleTreeFacts, D: ResolveFacts>(
    facts: &F,
    decls: &D,
    caller: &str,
    callee: &str,
    member: &str,
    target: ResolveTarget,
) -> Option<u32> {
    let TreeItem::Found { file, item } = module_tree_item(facts, caller, callee)? else {
        return None;
    };
    let file = file.as_ref();
    decls.first_decl_in_file(file, &item, target).or_else(|| {
        let bare = member.split('<').next().unwrap_or(member);
        decls.first_decl_in_file(file, bare, target)
    })
}

/// [`QualifierFacts::crate_module`] through the module tree: `Local` unless
/// `module_path` names a module of a workspace crate; then that crate's file
/// that declares `member` of `target`'s kinds under its own name, past the
/// `pub use` chain.
pub fn crate_module<F: ModuleTreeFacts, D: ResolveFacts>(
    facts: &F,
    decls: &D,
    source: &str,
    module_path: &str,
    member: &str,
    target: ResolveTarget,
) -> CrateModule<F::File> {
    let head = module_path.split("::").next().unwrap_or_default();
    if matches!(head, "crate" | "self" | "super")
        || facts.crate_root(head).is_none()
        || module_tree_module(facts, source, module_path).is_none()
    {
        return CrateModule::Local;
    }
    CrateModule::Placed(
        match module_tree_item(facts, source, &format!("{module_path}::{member}")) {
            Some(TreeItem::Found { file, item })
                if item == member
                    && decls
                        .first_decl_in_file(file.as_ref(), &item, target)
                        .is_some() =>
            {
                Some(file)
            }
            _ => None,
        },
    )
}

/// R12, Python `super().member()`: `member` on the caller class's bases,
/// by the owner index, never by bare name. Bases are tried in declared
/// order (a single-pass approximation of the MRO); the first base that owns
/// the member wins, at [`Tier::TypeOwned`], or [`Tier::TypeHeritage`] when
/// it only inherits it. A base the project cannot see, an ambiguous one, or
/// one whose ownership is unknown stops the walk: it may own the member and
/// shadow every later base, so no edge beats a guess.
///
/// A first base that only inherits the member is not enough: C3 puts a
/// later base ahead of a shared ancestor (`class D(B, C)` with `B(A)`,
/// `C(A)` reads `D, B, C, A`), so a later base that reaches the member
/// through another owner, or may do so, leaves no edge. No base, or a call
/// outside a class, is no edge.
pub fn super_member<'h, F: QualifierFacts>(
    facts: &F,
    site: QualifiedSite<'_>,
    heritage: &'h [String],
) -> (Resolution, Option<&'h str>) {
    let decls = facts.decls();
    let (member, target) = (site.member, site.target);
    let mut bases = heritage.iter();
    let mut owner: Option<(&'h String, u32, bool)> = None;
    for base in bases.by_ref() {
        let TypeCandidates::Unique(ty, ty_name) = super_base_type(facts, site, base) else {
            break;
        };
        match member_ownership(decls, ty, ty_name, member, target) {
            Ownership::Owned { id, inherited } => {
                owner = Some((base, id, inherited));
                break;
            }
            Ownership::NotOwned => {}
            Ownership::Unknown => break,
        }
    }
    // Can a later base come first in the C3 order with another `member`? A
    // base outside the project cannot derive from a project class, so it
    // never does; an ambiguous base, or one whose ownership is unknown, may.
    let later_may_shadow =
        |later: &str, owner_member: u32| match super_base_type(facts, site, later) {
            TypeCandidates::Unique(ty, ty_name) => {
                match member_ownership(decls, ty, ty_name, member, target) {
                    Ownership::Owned { id, .. } => id != owner_member,
                    Ownership::NotOwned => false,
                    Ownership::Unknown => true,
                }
            }
            TypeCandidates::Set(..) => true,
            TypeCandidates::External | TypeCandidates::None => false,
        };
    let owner = owner.filter(|&(_, id, inherited)| {
        !inherited || !bases.any(|later| later_may_shadow(later.as_str(), id))
    });
    match owner {
        Some((base, id, inherited)) => {
            let tier = if inherited {
                Tier::TypeHeritage
            } else {
                Tier::TypeOwned
            };
            (Resolution::bound(id, tier), Some(base.as_str()))
        }
        None => (Resolution::NoEdge(NoEdge::Unresolved), None),
    }
}

/// The project type a Python base expression names. A dotted base
/// (`base.Base` under `import base`) resolves its module through
/// [`qualifier_file`], then the type inside that module; when the module is
/// in the project but does not declare the type (a package re-export), the
/// last segment goes through [`QualifierFacts::type_candidates`]. A module
/// the project does not hold (`threading.Thread`) gives `None`: a project
/// `Thread` is not that base.
fn super_base_type<'q, F: QualifierFacts>(
    facts: &F,
    site: QualifiedSite<'q>,
    base: &'q str,
) -> TypeCandidates<'q> {
    let Some((qualifier, type_name)) = split_qualifier(base) else {
        return facts.type_candidates(site.source, base, site.imports);
    };
    let Some(module_file) = qualifier_file(
        facts,
        site.source,
        site.caller,
        site.imports,
        (Some(base), qualifier, type_name),
        ResolveTarget::Type,
        StemFile::Any,
    ) else {
        return TypeCandidates::None;
    };
    match facts
        .decls()
        .first_decl_in_file(module_file.as_ref(), type_name, ResolveTarget::Type)
    {
        Some(id) => TypeCandidates::Unique(id, type_name),
        None => facts.type_candidates(site.source, type_name, site.imports),
    }
}

/// One call site's own inputs to [`call`].
#[derive(Debug, Clone, Copy)]
pub struct CallContext<'c> {
    /// The caller's file, as the side stores it.
    pub source: &'c str,
    pub language: Language,
    pub imports: &'c [RawImport],
    /// The declared bases of the caller's enclosing type.
    pub heritage: &'c [String],
    /// The type the calling member belongs to, as the side keys it.
    pub owner: Option<&'c str>,
    /// The calling declaration.
    pub caller: Option<u32>,
}

/// What [`call`] reads per side (R1). Each method answers a question about
/// the side's declarations or runs the side's name tiers; [`call`] decides
/// the order and every fallback. Each side keeps its decision dump (index)
/// and its memos and gap notes (overlay) inside its methods.
pub trait CallFacts<'s> {
    /// The module facts the shared normalization rules read (R2-R5).
    type Modules: ModuleFacts + ?Sized;
    /// The declarations the post-filter and the fallbacks weigh.
    type Decls: PickFacts + ?Sized;
    /// How this side names the type of a qualified constructed type path.
    const TYPE_NAME: TypeName;
    fn modules(&self) -> &Self::Modules;
    fn decls(&self) -> &Self::Decls;
    /// The project types `receiver` can denote in `source` (step 1 of the
    /// receiver-typing ladder).
    fn type_candidates<'q>(
        &self,
        source: &str,
        receiver: &'q str,
        imports: &'q [RawImport],
    ) -> TypeCandidates<'q>;
    /// The side's ladder for `site` at the call `call`, with `heritage`:
    /// under the name `typed` (`Receiver.member`) when the receiver names a
    /// type, else under the site's own callee.
    fn ladder(
        &self,
        site: CallSite<'s>,
        typed: Option<&str>,
        call: CallPick<'_>,
        heritage: &[String],
    ) -> Resolution;
    /// The C# access rule ([`crate::analyzer::csharp_access`]) on the bound
    /// `id` of `site`: `id`, the one later overload C# admits in its place,
    /// or `None`.
    fn csharp_admitted(&self, id: u32, site: CallSite<'_>) -> Option<u32>;
    /// The bare `member` through the name tiers outside a call scope, with
    /// `heritage`, of `target`'s kinds.
    fn bare_member(
        &self,
        member: &str,
        target: ResolveTarget,
        heritage: &[String],
    ) -> Option<(u32, f32)>;
    /// Whether a constructible type and a `Method` the caller's file admits
    /// are both named `callee`.
    fn names_type_and_member(&self, callee: &str) -> bool;
    /// Whether some Class / Struct is declared as `name`.
    fn declares_constructible(&self, name: &str) -> bool;
    /// The type `path` names at the call `call`, with `heritage`, through
    /// the side's ladder; `bind_imports` says whether the import tiers may
    /// bind it.
    fn resolve_type(
        &self,
        path: &str,
        bind_imports: bool,
        call: CallPick<'_>,
        heritage: &[String],
    ) -> Option<(u32, f32)>;
    /// The `Calls` target of a construction of the type `type_id`: its one
    /// constructor, else the type.
    fn construction_target(&self, type_id: u32) -> u32;
    /// The decision dump: the import `import` bound `name` to `id` ahead of
    /// the ladder.
    fn note_import_bound(&self, _name: &str, _import: &RawImport, _id: u32) {}
    /// The decision dump: no tier may bind the C# untyped member `name`.
    fn note_untyped_unresolved(&self, _name: &str) {}
}

/// [`call`]'s verdict, with the site its ladder read.
#[derive(Debug, Clone, Copy)]
pub struct CallVerdict<'s> {
    pub resolution: Resolution,
    /// The site after normalization (R2): a Go value or unmapped package
    /// re-classified, a Swift, C# or Python receiver that names no type
    /// read as an untyped member.
    pub site: CallSite<'s>,
    /// Whether the receiver named a type (`Receiver.member`).
    pub typed: bool,
}

/// R1, the call entry of the index (`Resolver::resolve_call`) and the
/// overlay (`resolve_edges`): one call site's `Calls` target.
///
/// 1. Normalize the site (R2-R5). A Python from-imported submodule and a
///    mapped Go package settle the call ahead of the ladder; a Go value or
///    unmapped package re-classifies the site. A C#, Swift, Kotlin or Dart
///    receiver that names a type, or a Python one on a from-imported type,
///    resolves as `Receiver.member` ([`type_receiver`]). A Swift or C#
///    receiver that names none is an untyped member call, never `this.m()`
///    (codex ruling P-recv Q1); so is a Python receiver whose root is a
///    value its module declares.
/// 2. Derive the call pick from the normalized site: an untyped receiver is
///    no `this`, so no tier reaches the member through the caller's bases.
///    Run the ladder, except for a C# value of unknown type, which no tier
///    reaches by its name alone (codex ruling P-recv Q3).
/// 3. The C# access rule (codex ruling P-ext2 C).
/// 4. R17: a Kotlin typed receiver the ladder left without an edge retries
///    the bare member: `fun Reg.ext()` is a top-level function no member
///    lookup on `Reg` finds.
/// 5. R21: an untyped member call reaches a method or a type's function
///    ([`member_call_admits`]), or in Python another module's function.
/// 6. R22: the constructor fallback ([`constructed_type`]), unless the site
///    may be a member call whose receiver the parser dropped.
///
/// Steps 4 and 6 run where [`Resolution::call_fallbacks_allowed`] allows
/// them (B §3.2).
pub fn call<'s, F: CallFacts<'s>>(
    facts: &F,
    cx: CallContext<'_>,
    site: CallSite<'s>,
) -> CallVerdict<'s> {
    let language = cx.language;
    // A chain the caller did not type resolves as the entry the parser
    // wrote before chains.
    let mut site = site.fallback();
    let imports = || cx.imports.iter().map(ImportRef::from);
    let mut resolution = Resolution::Unknown;
    if let CallSite::FromReceiver { receiver, member } = site {
        match language {
            Language::Python => {
                if let Some((id, position)) = reexport::python_submodule_member(
                    facts.modules(),
                    cx.source,
                    (receiver, member),
                    imports(),
                    call_target(language, site),
                ) {
                    facts.note_import_bound(member, &cx.imports[position], id);
                    resolution = Resolution::bound(id, Tier::ImportScoped);
                }
            }
            Language::Go => match reexport::go_package_lookup(
                facts.modules(),
                cx.source,
                receiver,
                member,
                imports(),
            ) {
                GoPackageLookup::Resolved(target) => {
                    let bound = target.and_then(|(id, source)| {
                        Some((id, cx.imports.iter().find(|i| i.source == source)?))
                    });
                    resolution = match bound {
                        Some((id, import)) => {
                            facts.note_import_bound(member, import, id);
                            Resolution::bound(id, Tier::ImportScoped)
                        }
                        None => Resolution::NoEdge(NoEdge::PackageMissing),
                    };
                }
                GoPackageLookup::Unmapped => site = CallSite::Plain(member),
                GoPackageLookup::Value => site = CallSite::UntypedMember(member),
            },
            _ => {}
        }
    }
    // A call settled ahead of the ladder is neither typed nor
    // re-classified: those rules only shape the ladder's lookup.
    let open = matches!(resolution, Resolution::Unknown);
    let typed = if open {
        type_receiver(facts, cx.source, language, cx.imports, site)
    } else {
        None
    };
    let site = match (site, &typed) {
        (CallSite::FromReceiver { member, .. }, None)
            if matches!(language, Language::Swift | Language::CSharp) =>
        {
            CallSite::UntypedMember(member)
        }
        (CallSite::FromReceiver { receiver, member }, None)
            if open
                && language == Language::Python
                && receiver.contains('/')
                && reexport::python_from_import_declared_attribute(
                    facts.modules(),
                    cx.source,
                    receiver,
                    imports(),
                ) =>
        {
            CallSite::UntypedMember(member)
        }
        _ => site,
    };
    let untyped = matches!(site, CallSite::UntypedMember(_));
    let heritage: &[String] = if untyped { &[] } else { cx.heritage };
    let pick = CallPick {
        owner: cx.owner,
        caller: cx.caller,
        untyped,
        free_only: site.reaches_free_only(language),
    };
    if open && untyped && language == Language::CSharp {
        facts.note_untyped_unresolved(site.name());
    } else if open {
        resolution = facts.ladder(site, typed.as_deref(), pick, heritage);
    }
    if language == Language::CSharp {
        resolution = resolution.admit_or(|id| facts.csharp_admitted(id, site));
    }
    if resolution.call_fallbacks_allowed() && typed.is_some() && language == Language::Kotlin {
        let target = call_target(language, site);
        resolution = Resolution::from_ladder(
            extension_fallback(facts.decls(), site.name(), |member| {
                facts.bare_member(member, target, heritage)
            })
            .next(),
        );
    }
    if untyped {
        let decls = facts.decls();
        // A Python module is a value: `api.helper()` on a receiver the parser
        // could not type may call another module's function. No value
        // reaches the caller's own module that way.
        resolution = resolution.admit(|id| {
            member_call_admits(decls, id, cx.owner)
                || (language == Language::Python
                    && is_free(decls, id)
                    && decls.file_of(id) != Some(cx.source))
        });
    }
    if resolution.call_fallbacks_allowed() && !may_be_receiverless_member_call(facts, site) {
        let constructed = constructed_type(
            facts.decls(),
            language,
            site,
            cx.imports,
            F::TYPE_NAME,
            |name| facts.declares_constructible(name),
            |path, bind_imports| facts.resolve_type(path, bind_imports, pick, heritage),
        );
        if let Some((type_id, confidence)) = constructed {
            resolution =
                Resolution::from_ladder(Some((facts.construction_target(type_id), confidence)));
        }
    }
    CallVerdict {
        resolution,
        site,
        typed: typed.is_some(),
    }
}

/// The kinds a call site may reach. Rust method syntax (`x.f()`) calls only
/// methods: a free `fn` is never in scope through `.`.
fn call_target(language: Language, site: CallSite<'_>) -> ResolveTarget {
    if site.uses_method_syntax() && language == Language::Rust {
        ResolveTarget::Method
    } else {
        ResolveTarget::Callable
    }
}

/// R2: the callee of a C#, Swift, Kotlin or Dart member call on an
/// identifier receiver that is no local, parameter, field or property in
/// scope ([`CallSite::FromReceiver`]). A receiver that names a project type,
/// a `using` alias of one, or a library type resolves as `Receiver.member`
/// through the qualifier tiers and the receiver-typing ladder, and never by
/// the bare member: `StringAssert.AreEqual()` must not land on
/// `XUnitAssert.AreEqual`, nor Swift `Util.make()` on the caller's own
/// `make`. A receiver that names no type is a member the parser cannot see
/// (an inherited property, a global), or in C# also a library type
/// (`Console`): `None`.
///
/// A Python `FromReceiver` that is no submodule ([`call`] binds those
/// first) names a type only when its module file declares one
/// ([`reexport::python_imported_type`]): `from k import K; K.st()` with
/// several project `st` would otherwise bind none.
pub fn type_receiver<'s, F: CallFacts<'s>>(
    facts: &F,
    source: &str,
    language: Language,
    imports: &[RawImport],
    site: CallSite<'_>,
) -> Option<String> {
    let CallSite::FromReceiver { receiver, member } = site else {
        return None;
    };
    let typed = match language {
        Language::Python => reexport::python_imported_type(
            facts.modules(),
            source,
            receiver,
            imports.iter().map(ImportRef::from),
        ),
        Language::CSharp | Language::Swift | Language::Kotlin | Language::Dart => !matches!(
            facts.type_candidates(source, receiver, imports),
            TypeCandidates::None
        ),
        _ => false,
    };
    typed.then(|| format!("{receiver}.{member}"))
}

/// Whether a bare `m()` that the constructor fallback would read as a
/// construction of a type `m` may instead be a member call whose receiver
/// the parser dropped. The C++, Swift and Ruby parsers write an untyped
/// receiver as [`CallSite::UntypedMember`], which never constructs; a
/// parser that still drops one (Kotlin and Dart write a receiver they bind
/// to a field or local as the bare member) reaches here. When an admitted
/// `Method` is named `m`, the name is a member's, and `m(x)` never binds a
/// type `m` of another file (`template <typename T> class number_integer`
/// in a test file for nlohmann's `sax->number_integer(v)`). A call written
/// with a qualifier, a `new`, or a Ruby `.new` is no such site.
fn may_be_receiverless_member_call<'s, F: CallFacts<'s>>(facts: &F, site: CallSite<'_>) -> bool {
    matches!(site, CallSite::Plain(callee)
        if !callee.contains(['.', ':']) && facts.names_type_and_member(callee))
}

/// Which name of a qualified constructed type path ([`constructed_type`])
/// the gate and the last-segment retry read. The two sides differ on a path
/// with an empty qualifier (C++ `new ::Widget()`, Ruby `::Widget.new`); the
/// parameter keeps each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeName {
    /// The member [`split_qualifier`] gives. A path whose qualifier is
    /// empty (`::Widget`) names itself, so no type passes the gate: the
    /// index.
    Qualifier,
    /// The text after the last `.`, `:` or `\`: `::Widget` names `Widget`,
    /// which the last-segment retry may bind: the overlay.
    LastSegment,
}

/// R22, the constructor fallback: the Class / Struct a call site
/// constructs, with the confidence of the lookup that found it; the caller
/// lands the edge on its construction target. `resolve` looks a type path
/// up through the side's tiers; its second argument says whether the import
/// tiers may bind it.
///
/// `declares_constructible` is the cheap gate: some Class / Struct is
/// declared under the type's name, or an import aliases a declared one to it
/// (`import { Widget as Renamed }`), which only the import tier maps back.
/// The whole type path resolves first: `u.Widget()` binds through `import pkg
/// as u`, `Outer.Inner()` through the qualifier tier. A qualified type path
/// that does not resolve (`new shop.Widget()` through a namespace import,
/// PHP `new \App\Item()`) retries its last segment only where
/// [`CallSite::constructed_type`] allows it, and never through an import in a
/// fully qualified language: `new \App\User()` never names its type through a
/// `use` of its last segment. `split` names the type of the path
/// ([`TypeName`]).
pub fn constructed_type<F: PickFacts + ?Sized>(
    decls: &F,
    language: Language,
    site: CallSite<'_>,
    imports: &[RawImport],
    split: TypeName,
    declares_constructible: impl Fn(&str) -> bool,
    mut resolve: impl FnMut(&str, bool) -> Option<(u32, f32)>,
) -> Option<(u32, f32)> {
    let (type_path, last_segment_fallback) = site.constructed_type(language)?;
    let type_name = match split {
        TypeName::Qualifier => split_qualifier(type_path).map_or(type_path, |(_, member)| member),
        TypeName::LastSegment => type_path
            .rsplit(['.', ':', '\\'])
            .next()
            .unwrap_or(type_path),
    };
    let names_type = declares_constructible(type_name)
        || imports.iter().any(|import| {
            import.alias.as_deref() == Some(type_name)
                && declares_constructible(&import.imported_name)
        });
    if !names_type {
        return None;
    }
    let qualified = type_name.len() < type_path.len();
    let (type_id, confidence) = resolve(type_path, true).or_else(|| {
        (qualified && last_segment_fallback).then(|| resolve(type_name, !fqn_language(language)))?
    })?;
    decls
        .kind(type_id)
        .is_constructible()
        .then_some((type_id, confidence))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::outcome::Hit;
    use crate::analyzer::reexport::{ExportAliasTable, Fact, FileImports, PyModule};
    use crate::analyzer::same_file_pick::ImplMember;
    use std::borrow::Cow;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    /// How a fake stores its declarations: the index's per-file name table,
    /// or the overlay's flat scan over its nodes.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Layout {
        Index,
        Overlay,
    }

    /// One declaration: `(file, name, kind, owner)`, id = position.
    type Decl = (&'static str, &'static str, NodeKind, Option<&'static str>);

    struct Fake {
        layout: Layout,
        decls: Vec<Decl>,
        /// `(file, name)` -> ids in source order, for [`Layout::Index`].
        by_file: HashMap<(&'static str, &'static str), Vec<u32>>,
        scoped: Vec<u32>,
        /// `(type id, resolved bases)`.
        supertypes: Vec<(u32, Vec<u32>)>,
        /// Every `first_decl_in_file` probe, as `(file, name)`.
        probes: RefCell<Vec<(String, String)>>,
    }

    impl Fake {
        fn new(layout: Layout, decls: &[Decl]) -> Self {
            let mut by_file: HashMap<_, Vec<u32>> = HashMap::new();
            for (id, &(file, name, _, _)) in decls.iter().enumerate() {
                by_file.entry((file, name)).or_default().push(id as u32);
            }
            Self {
                layout,
                decls: decls.to_vec(),
                by_file,
                scoped: Vec::new(),
                supertypes: Vec::new(),
                probes: RefCell::default(),
            }
        }

        fn ids(&self) -> impl Iterator<Item = u32> + '_ {
            0..self.decls.len() as u32
        }

        /// `file`'s declarations of `name`, scoped ones included.
        fn in_file(&self, file: &str, name: &str) -> Vec<u32> {
            match self.layout {
                Layout::Index => self
                    .by_file
                    .iter()
                    .find(|(key, _)| key.0 == file && key.1 == name)
                    .map_or_else(Vec::new, |(_, ids)| ids.clone()),
                Layout::Overlay => self
                    .ids()
                    .filter(|&id| {
                        let (f, n, _, _) = self.decls[id as usize];
                        f == file && n == name
                    })
                    .collect(),
            }
        }

        fn meta(&self, id: u32) -> FileMeta {
            FileMeta::from_path(self.decls[id as usize].0)
        }

        /// Every declaration of `name`, with its file's meta: the Tier 3
        /// candidate stream.
        fn named(&self, name: &str) -> Vec<(u32, FileMeta)> {
            self.ids()
                .filter(|&id| self.decls[id as usize].1 == name)
                .map(|id| (id, self.meta(id)))
                .collect()
        }
    }

    impl PickFacts for Fake {
        fn pick_candidates(&self, file: &str, name: &str) -> Cow<'_, [u32]> {
            let mut ids = self.in_file(file, name);
            ids.retain(|id| !self.scoped.contains(id));
            Cow::Owned(ids)
        }
        fn kind(&self, id: u32) -> NodeKind {
            self.decls[id as usize].2
        }
        fn owner(&self, id: u32) -> Option<&str> {
            self.decls[id as usize].3
        }
        fn impl_member(&self, _id: u32) -> Option<ImplMember<'_>> {
            None
        }
        fn is_namespace_owner(&self, _file: &str, _owner: &str) -> bool {
            false
        }
        fn file_of(&self, id: u32) -> Option<&str> {
            self.decls.get(id as usize).map(|decl| decl.0)
        }
        fn types_named(&self, owner: &str) -> Vec<u32> {
            self.ids()
                .filter(|&id| self.decls[id as usize].1 == owner && self.kind(id).is_type())
                .collect()
        }
        fn is_lexically_scoped(&self, id: u32) -> bool {
            self.scoped.contains(&id)
        }
        fn ancestors(&self, ty: u32) -> Vec<u32> {
            self.supertypes
                .iter()
                .find(|(t, _)| *t == ty)
                .map_or_else(Vec::new, |(_, bases)| bases.clone())
        }
    }

    impl TypeFacts for Fake {
        fn supertypes(&self, ty: u32) -> Option<(Cow<'_, [u32]>, Option<u32>)> {
            self.supertypes
                .iter()
                .find(|(t, _)| *t == ty)
                .map(|(_, bases)| (Cow::Borrowed(bases.as_slice()), None))
        }
        fn name_of(&self, id: u32) -> Option<&str> {
            self.decls.get(id as usize).map(|decl| decl.1)
        }
        fn owned_in_file(
            &self,
            file: &str,
            member: &str,
            owner: &str,
            target: ResolveTarget,
        ) -> Vec<u32> {
            let wanted = target.kind_predicate();
            self.in_file(file, member)
                .into_iter()
                .filter(|&id| wanted(self.kind(id)) && self.owner(id) == Some(owner))
                .collect()
        }
        fn owned_global(
            &self,
            member: &str,
            owner: &str,
            target: ResolveTarget,
            _scope_file: &str,
        ) -> Vec<u32> {
            let wanted = target.kind_predicate();
            self.ids()
                .filter(|&id| {
                    self.decls[id as usize].1 == member
                        && wanted(self.kind(id))
                        && self.owner(id) == Some(owner)
                })
                .collect()
        }
        fn count_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> usize {
            let wanted = target.kind_predicate();
            self.in_file(file, name)
                .into_iter()
                .filter(|&id| wanted(self.kind(id)))
                .count()
        }
        fn global_candidates(
            &self,
            name: &str,
            target: ResolveTarget,
            caller: FileMeta,
        ) -> Vec<u32> {
            let wanted = target.kind_predicate();
            self.named(name)
                .into_iter()
                .filter(|&(id, meta)| {
                    wanted(self.kind(id)) && caller.admits(meta) && !self.is_lexically_scoped(id)
                })
                .map(|(id, _)| id)
                .collect()
        }
        /// Same kind and owner, in another file of the same file name.
        fn is_copy(&self, a: u32, b: u32) -> bool {
            let (a_decl, b_decl) = (self.decls[a as usize], self.decls[b as usize]);
            let leaf = |path: &'static str| path.rsplit('/').next();
            a_decl.0 != b_decl.0
                && a_decl.2 == b_decl.2
                && a_decl.3 == b_decl.3
                && leaf(a_decl.0) == leaf(b_decl.0)
        }
        /// The copy whose path shares the longest prefix with the caller's.
        fn nearest_copy(&self, caller_file: &str, candidates: &[u32]) -> Option<u32> {
            if candidates.len() < 2 {
                return None;
            }
            let shared = |id: u32| {
                caller_file
                    .bytes()
                    .zip(self.decls[id as usize].0.bytes())
                    .take_while(|(a, b)| a == b)
                    .count()
            };
            let best = candidates.iter().copied().max_by_key(|&id| shared(id))?;
            let ties = candidates
                .iter()
                .filter(|&&id| shared(id) == shared(best))
                .count();
            (ties == 1).then_some(best)
        }
        fn return_type_of(&self, _callee: u32) -> Option<&str> {
            None
        }
    }

    impl ResolveFacts for Fake {
        fn call_decls(
            &self,
            file: &str,
            name: &str,
        ) -> impl Iterator<Item = (u32, NodeKind)> + Clone {
            self.in_file(file, name)
                .into_iter()
                .filter(|id| !self.scoped.contains(id))
                .map(|id| (id, self.kind(id)))
                .collect::<Vec<_>>()
                .into_iter()
        }
        fn first_decl_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> Option<u32> {
            self.probes
                .borrow_mut()
                .push((file.to_owned(), name.to_owned()));
            let wanted = target.kind_predicate();
            self.in_file(file, name)
                .into_iter()
                .find(|&id| wanted(self.kind(id)))
        }
        fn has_named(&self, name: &str) -> bool {
            self.decls.iter().any(|decl| decl.1 == name)
        }
        fn enum_variant_of(&self, type_id: u32, type_name: &str, variant: &str) -> Option<u32> {
            let file = self.file_of(type_id)?;
            let declared = self.name_of(type_id)?;
            let owned_by = |owner: &str| {
                self.in_file(file, variant).into_iter().find(|&id| {
                    self.kind(id) == NodeKind::EnumVariant && self.owner(id) == Some(owner)
                })
            };
            owned_by(type_name).or_else(|| owned_by(declared))
        }
    }

    const F: NodeKind = NodeKind::Function;
    const M: NodeKind = NodeKind::Method;
    const C: NodeKind = NodeKind::Class;
    const CALLABLE: ResolveTarget = ResolveTarget::Callable;

    fn both(decls: &[Decl]) -> [Fake; 2] {
        [
            Fake::new(Layout::Index, decls),
            Fake::new(Layout::Overlay, decls),
        ]
    }

    fn bound(id: u32, tier: Tier) -> Resolution {
        Resolution::Bound(Hit {
            id,
            confidence: tier.confidence(),
            tier: Some(tier),
        })
    }

    /// Tier 1 reads the same facts through either store layout and gives
    /// one `Resolution` per case: what the index and the overlay emit for a
    /// clean caller and for the same caller once dirty.
    #[test]
    fn test_ladder_tier1_same_file_binds_both_facts() {
        let decls: [Decl; 6] = [
            ("a.py", "helper", F, None),
            ("a.py", "helper", M, Some("K")),
            ("a.py", "m", M, Some("A")),
            ("a.py", "m", M, Some("B")),
            ("a.py", "only", M, Some("K")),
            ("a.py", "solo", F, None),
        ];
        let owner_k = CallPick {
            owner: Some("K"),
            ..CallPick::default()
        };
        let free_only = CallPick {
            free_only: true,
            ..CallPick::default()
        };
        let untyped = CallPick {
            untyped: true,
            ..CallPick::default()
        };
        let cases: [(&str, Option<CallPick<'_>>, Resolution); 10] = [
            // No call site: the first callable in source order.
            ("helper", None, bound(0, Tier::SameFile)),
            // A free function reached without a receiver stays the pick.
            ("helper", Some(owner_k), bound(0, Tier::SameFile)),
            ("helper", Some(free_only), bound(0, Tier::SameFile)),
            // Two same-file candidates of unrelated owners: no pick.
            ("m", Some(CallPick::default()), Resolution::Unknown),
            // The caller's own type owns the member.
            (
                "m",
                Some(CallPick {
                    owner: Some("B"),
                    ..CallPick::default()
                }),
                bound(3, Tier::SameFile),
            ),
            // A bare call that reaches no member weighs no method.
            ("only", Some(free_only), Resolution::Unknown),
            // A candidate list of one; an untyped receiver never reaches a
            // free function.
            ("solo", Some(CallPick::default()), bound(5, Tier::SameFile)),
            ("solo", Some(untyped), Resolution::Unknown),
            // An empty candidate list, and a whitespace-only name.
            ("absent", None, Resolution::Unknown),
            ("  ", Some(owner_k), Resolution::Unknown),
        ];
        let [index, overlay] = both(&decls);
        for (name, call, want) in cases {
            let got =
                [&index, &overlay].map(|facts| same_file(facts, "a.py", name, CALLABLE, call, &[]));
            assert_eq!(got, [want, want], "{name:?} {call:?}");
        }
        assert_eq!(
            same_file(&index, "a.py", "helper", CALLABLE, None, &[]).edge(),
            Some((0, 1.0)),
            "Tier 1 keeps confidence 1.0"
        );
    }

    /// Tier 1 skips a scoped declaration (I5) and binds its unscoped twin.
    #[test]
    fn test_ladder_tier1_scoped_declaration_returns_unscoped_twin() {
        for mut facts in both(&[("b.py", "helper", F, None), ("b.py", "helper", F, None)]) {
            facts.scoped = vec![0];
            assert_eq!(
                same_file(&facts, "b.py", "helper", CALLABLE, None, &[]),
                bound(1, Tier::SameFile)
            );
        }
    }

    #[test]
    fn test_ladder_global_ambiguous_returns_noedge_with_count() {
        let decls: [Decl; 4] = [
            ("a/x.py", "helper", F, None),
            ("b/y.py", "helper", F, None),
            ("c/z.py", "helper", F, None),
            ("vendor/v/w.py", "helper", F, None),
        ];
        let caller = FileMeta::from_path("app.py");
        let [mut facts, _] = both(&decls);
        let candidates = facts.named("helper");
        let run = |facts: &Fake, nearest| {
            global(
                facts,
                "app.py",
                caller,
                CALLABLE,
                nearest,
                || candidates.iter().copied(),
                |_| false,
            )
        };
        // The vendor candidate does not fit a project caller: three do.
        assert_eq!(run(&facts, true), Resolution::NoEdge(NoEdge::Ambiguous(3)));
        assert_eq!(run(&facts, false), Resolution::NoEdge(NoEdge::Ambiguous(3)));
        // A scoped candidate fits no name tier (I5).
        facts.scoped = vec![2];
        assert_eq!(run(&facts, true), Resolution::NoEdge(NoEdge::Ambiguous(2)));
        assert!(run(&facts, true).call_fallbacks_allowed());
        assert_eq!(run(&facts, true).edge(), None);
    }

    /// The other Tier 3 verdicts: an empty candidate list, one candidate,
    /// a hidden pick, a pick outside the call's kinds, and copies.
    #[test]
    fn test_ladder_global_unique_hidden_kind_and_copies_returns_each_verdict() {
        let caller = FileMeta::from_path("core/pkg/app.py");
        let run = |facts: &Fake, target, nearest, hidden: &dyn Fn(u32) -> bool| {
            let candidates = facts.named("helper");
            global(
                facts,
                "core/pkg/app.py",
                caller,
                target,
                nearest,
                || candidates.iter().copied(),
                hidden,
            )
        };
        let none = |_: u32| false;

        let empty = Fake::new(Layout::Index, &[]);
        assert_eq!(
            run(&empty, CALLABLE, true, &none),
            Resolution::NoEdge(NoEdge::Unresolved)
        );

        let one = Fake::new(Layout::Index, &[("lib/util.py", "helper", F, None)]);
        assert_eq!(run(&one, CALLABLE, true, &none), bound(0, Tier::Global));
        assert_eq!(
            run(&one, CALLABLE, true, &none).edge(),
            Some((0, 0.7)),
            "Tier 3 keeps confidence 0.7"
        );
        assert_eq!(
            run(&one, CALLABLE, true, &|id: u32| id == 0),
            Resolution::NoEdge(NoEdge::Hidden)
        );
        assert_eq!(
            run(&one, ResolveTarget::Method, true, &none),
            Resolution::NoEdge(NoEdge::Unresolved),
            "Rust method syntax never binds a free fn"
        );

        let copies = Fake::new(
            Layout::Index,
            &[
                ("android/core/pkg/util.py", "helper", F, None),
                ("core/pkg/util.py", "helper", F, None),
            ],
        );
        assert_eq!(
            run(&copies, CALLABLE, true, &none),
            bound(1, Tier::NearestCopy)
        );
        assert_eq!(run(&copies, CALLABLE, true, &none).edge(), Some((1, 0.6)));
        assert_eq!(
            run(&copies, CALLABLE, false, &none),
            Resolution::NoEdge(NoEdge::Ambiguous(2)),
            "no call site: no nearest copy"
        );
        assert_eq!(
            run(&copies, CALLABLE, true, &|id: u32| id == 1),
            Resolution::NoEdge(NoEdge::Ambiguous(2)),
            "a hidden copy binds nothing"
        );
        assert_eq!(
            nearest_copy(&copies, "core/pkg/app.py", caller, copies.named("helper")),
            Some(1)
        );
    }

    /// The heritage tier probes each declared base once, in declared order,
    /// and stops at the first that owns the member. A supertype cycle and a
    /// base absent from the graph both end the walk.
    #[test]
    fn test_ladder_heritage_walks_supertype_once_per_level() {
        let decls: [Decl; 3] = [
            ("a.py", "A", C, None),
            ("a.py", "m", M, Some("A")),
            ("b.py", "B", C, None),
        ];
        let file_of_base = |base: &str| match base {
            "A" => Some("a.py"),
            "B" => Some("b.py"),
            _ => None,
        };
        let walk = |facts: &Fake, bases: &[String], call: Option<CallPick<'_>>| {
            let seen = RefCell::new(Vec::new());
            let (resolution, base) = heritage(facts, bases, "m", CALLABLE, call, |base| {
                seen.borrow_mut().push(base.to_owned());
                file_of_base(base)
            });
            (resolution, base.map(str::to_owned), seen.into_inner())
        };
        let names = |bases: &[&str]| bases.iter().map(|b| b.to_string()).collect::<Vec<_>>();

        for mut facts in both(&decls) {
            // `B`'s file declares no `m`: the walk goes on to `A`.
            let bases = names(&["B", "A"]);
            let (resolution, base, seen) = walk(&facts, bases.as_slice(), None);
            assert_eq!(resolution, bound(1, Tier::HeritageScoped));
            assert_eq!(resolution.edge(), Some((1, 0.8)));
            assert_eq!(base.as_deref(), Some("A"));
            assert_eq!(seen, names(&["B", "A"]));

            // The first base that owns the member decides.
            let bases = names(&["A", "B"]);
            let (_, base, seen) = walk(&facts, bases.as_slice(), None);
            assert_eq!((base.as_deref(), seen), (Some("A"), names(&["A"])));

            // A cycle (`A: B`, `B: A`): only the declared bases are probed.
            facts.supertypes = vec![(0, vec![2]), (2, vec![0])];
            let bases = names(&["B"]);
            let (resolution, base, seen) = walk(&facts, bases.as_slice(), None);
            assert_eq!((resolution, base), (Resolution::Unknown, None));
            assert_eq!(seen, names(&["B"]));

            // A base absent from the graph, and a whitespace-only one.
            let bases = names(&["Missing", "  ", "A"]);
            let (resolution, _, seen) = walk(&facts, bases.as_slice(), None);
            assert_eq!(resolution, bound(1, Tier::HeritageScoped));
            assert_eq!(seen, names(&["Missing", "  ", "A"]));

            // No bases; an untyped receiver; a call that reaches no member.
            let no_bases: Vec<String> = Vec::new();
            assert_eq!(
                walk(&facts, no_bases.as_slice(), None),
                (Resolution::Unknown, None, vec![])
            );
            let bases = names(&["A"]);
            for call in [
                CallPick {
                    untyped: true,
                    ..CallPick::default()
                },
                CallPick {
                    free_only: true,
                    ..CallPick::default()
                },
            ] {
                assert_eq!(
                    walk(&facts, bases.as_slice(), Some(call)),
                    (Resolution::Unknown, None, vec![]),
                    "{call:?}"
                );
            }
            let probes = facts.probes.borrow().len();
            assert_eq!(probes, 5, "one file probe per base that has a file");
        }
    }

    /// A base that no file declares costs one lookup and no file probe.
    #[test]
    fn test_ladder_heritage_unresolved_bases_returns_unknown_without_probe() {
        let facts = Fake::new(Layout::Overlay, &[("a.py", "m", M, Some("A"))]);
        let calls = Cell::new(0);
        let bases = vec!["X".to_string(), "Y".to_string()];
        let (resolution, base) = heritage(&facts, &bases, "m", CALLABLE, None, |_| {
            calls.set(calls.get() + 1);
            None::<&str>
        });
        assert_eq!((resolution, base), (Resolution::Unknown, None));
        assert_eq!(calls.get(), 2);
        assert!(facts.probes.borrow().is_empty());
    }

    const P: NodeKind = NodeKind::Property;
    const EV: NodeKind = NodeKind::EnumVariant;

    /// A plain field read takes Tier 1 for a field of the reader's file, the
    /// first of two same-named fields in source order, and Tier 3 for
    /// anything else; a qualified read goes to the qualifier tier.
    #[test]
    fn test_ladder_field_read_bare_and_qualified_returns_each_tier() {
        let decls: [Decl; 4] = [
            ("a.py", "timeout", P, Some("Config")),
            ("a.py", "timeout", P, Some("Other")),
            ("a.py", "run", M, Some("Config")),
            ("b.py", "remote", P, Some("K")),
        ];
        let tier1 = FieldRead::Bound(Hit {
            id: 0,
            confidence: 1.0,
            tier: Some(Tier::SameFile),
        });
        let cases: [(&str, FieldRead); 7] = [
            ("timeout", tier1),
            // Declared in another file only: Tier 3 decides.
            ("remote", FieldRead::Global),
            // A method of the name is no field.
            ("run", FieldRead::Global),
            ("absent", FieldRead::Global),
            ("  ", FieldRead::Global),
            ("c.timeout", FieldRead::Qualified),
            // An empty member is no qualified name.
            ("Config::", FieldRead::Global),
        ];
        for facts in both(&decls) {
            for (name, want) in cases {
                assert_eq!(field_read(&facts, "a.py", name), want, "{name:?}");
            }
        }
    }

    /// The lexical tier binds first at Tier 1's confidence and asks no name
    /// tier; a name the project never declares binds nothing.
    #[test]
    fn test_ladder_name_ref_lexical_hit_skips_name_tiers() {
        let facts = Fake::new(Layout::Index, &[("a.py", "helper", F, None)]);
        let asked = Cell::new(0);
        let run = |name: &'static str, lexical| {
            name_ref(
                &facts,
                "a.py",
                Language::Python,
                &[],
                None,
                NameRef {
                    name,
                    kind: crate::analyzer::types::NameRefKind::Value,
                },
                lexical,
                |_: &str, _| {
                    asked.set(asked.get() + 1);
                    vec![(0, 1.0)]
                },
                |_: &str, _| Vec::new(),
            )
        };
        assert_eq!(run("helper", Some(9)), Some((9, 1.0)));
        assert_eq!(asked.get(), 0, "a lexical hit reads no name tier");
        assert_eq!(run("missing", None), None);
        assert_eq!(asked.get(), 0, "an undeclared name reads no name tier");
        assert_eq!(run("helper", None), Some((0, 1.0)));
        assert_eq!(asked.get(), 1);
    }

    /// Every verdict of the bare-variant pick, for the store layouts of
    /// both sides.
    #[test]
    fn test_ladder_bare_variant_returns_each_verdict() {
        let decls: [Decl; 4] = [
            ("lib/a.c", "RED", EV, Some("Color")),
            ("lib/b.c", "BLUE", EV, Some("Color")),
            ("lib/c.c", "BLUE", NodeKind::Const, None),
            ("lib/d.c", "LIMIT", NodeKind::Const, None),
        ];
        let caller = FileMeta::from_path("app.c");
        for facts in both(&decls) {
            let pick = |name: &str, in_file: Option<Vec<(u32, NodeKind)>>| {
                bare_variant(
                    &facts,
                    name,
                    caller,
                    |name: &str| facts.decls.iter().any(|d| d.1 == name && d.2 == EV),
                    || in_file,
                    || facts.named(name),
                )
            };
            let unresolved = Resolution::NoEdge(NoEdge::Unresolved);
            // The reader's file decides when it declares the name.
            assert_eq!(pick("RED", Some(vec![(0, EV)])), bound(0, Tier::SameFile));
            assert_eq!(pick("RED", Some(vec![(5, NodeKind::Macro)])), unresolved);
            assert_eq!(pick("RED", Some(vec![(0, EV), (5, EV)])), unresolved);
            assert_eq!(pick("RED", Some(Vec::new())), unresolved);
            // Else the one project-wide value, when it is a variant.
            assert_eq!(pick("RED", None), bound(0, Tier::Global));
            assert_eq!(pick("RED", None).edge(), Some((0, 0.7)));
            assert_eq!(pick("BLUE", None), unresolved, "a constant shares the name");
            // No variant of the name: the gate stops before any store read.
            let read = Cell::new(false);
            let gated = bare_variant(
                &facts,
                "LIMIT",
                caller,
                |_: &str| false,
                || {
                    read.set(true);
                    None::<Vec<(u32, NodeKind)>>
                },
                || {
                    read.set(true);
                    facts.named("LIMIT")
                },
            );
            assert_eq!((gated, read.get()), (unresolved, false));
            assert_eq!(pick("  ", None), unresolved);
        }
    }

    /// A qualified variant binds the variant its qualifier's type owns: two
    /// enums with one variant name never mix; Rust `Self` reads the owner;
    /// an absent enum or a non-unique type binds nothing.
    #[test]
    fn test_ladder_qualified_variant_binds_owned_variant() {
        let decls: [Decl; 4] = [
            ("e.rs", "Color", NodeKind::Enum, None),
            ("e.rs", "Red", EV, Some("Color")),
            ("f.rs", "Shade", NodeKind::Enum, None),
            ("f.rs", "Red", EV, Some("Shade")),
        ];
        let types = |name: &str| match name {
            "Color" | "Alias" => Some((0, 0.95)),
            "Shade" => Some((2, 1.0)),
            _ => None,
        };
        let has_variant = |name: &str| name == "Red";
        for facts in both(&decls) {
            let asked = RefCell::new(Vec::new());
            let run =
                |language, owner: Option<&'static str>, path: (&'static str, &'static str)| {
                    qualified_variant(&facts, language, owner, path, has_variant, |q| {
                        asked.borrow_mut().push(q);
                        types(q)
                    })
                };
            assert_eq!(run(Language::Rust, None, ("Color", "Red")), Some((1, 0.95)));
            assert_eq!(run(Language::Rust, None, ("Shade", "Red")), Some((3, 1.0)));
            // An import alias of the enum still reaches its variant.
            assert_eq!(run(Language::Rust, None, ("Alias", "Red")), Some((1, 0.95)));
            assert_eq!(
                run(Language::Rust, Some("Shade"), ("Self", "Red")),
                Some((3, 1.0))
            );
            asked.borrow_mut().clear();
            assert_eq!(run(Language::Rust, None, ("Self", "Red")), None);
            assert_eq!(run(Language::Rust, None, ("Color", "Green")), None);
            assert!(asked.borrow().is_empty(), "no type lookup: {asked:?}");
            // `Self` is only Rust's; elsewhere it is the qualifier as written.
            assert_eq!(run(Language::Java, Some("Shade"), ("Self", "Red")), None);
            assert_eq!(asked.borrow().as_slice(), ["Self"]);
            assert_eq!(run(Language::Rust, None, ("Missing", "Red")), None);
        }
    }

    /// The import facts both sides answer: specifier -> module, the indexed
    /// module names, and `(file, name, owner)` members, id = position.
    #[derive(Default)]
    struct Imports {
        modules: Vec<(&'static str, Module<&'static str>)>,
        module_names: Vec<&'static str>,
        members: Vec<(&'static str, &'static str, MemberOwner<'static>)>,
        /// `(package module, name, id)` a Python package re-exports.
        reexports: Vec<(&'static str, &'static str, u32)>,
    }

    impl ImportModules for Imports {
        type Module = &'static str;

        fn import_module(
            &self,
            _importer: &str,
            specifier: &str,
            _language: Language,
        ) -> Module<&'static str> {
            self.modules
                .iter()
                .find(|(source, _)| *source == specifier)
                .map_or(Module::Missing, |(_, module)| module.clone())
        }

        fn module_name_indexed(&self, _language: Language, name: &str) -> bool {
            self.module_names.contains(&name)
        }
    }

    impl ImportMembers for Imports {
        fn member_in_file(
            &self,
            file: &str,
            name: &str,
            _target: ResolveTarget,
            owner: MemberOwner<'_>,
        ) -> Option<u32> {
            self.members
                .iter()
                .position(|&(f, n, o)| {
                    f == file && n == name && (owner == MemberOwner::Any || o == owner)
                })
                .map(|id| id as u32)
        }

        fn python_reexport(
            &self,
            language: Language,
            module: &str,
            name: &str,
            _target: ResolveTarget,
        ) -> Option<u32> {
            self.reexports
                .iter()
                .find(|&&(m, n, _)| language == Language::Python && m == module && n == name)
                .map(|&(_, _, id)| id)
        }
    }

    fn import(source: &str, imported_name: &str, alias: Option<&str>) -> RawImport {
        RawImport {
            source: source.to_owned(),
            imported_name: imported_name.to_owned(),
            alias: alias.map(str::to_owned),
            binding_kind: None,
        }
    }

    fn hit(id: u32, import: &RawImport) -> ImportHit<'_, &'static str> {
        ImportHit {
            id,
            import,
            barrel: None,
        }
    }

    const IMPORTER: &str = "app/main";

    /// Python binds the explicit import's member; a second import of the
    /// name never overrides the first hit. A member the module lacks, but
    /// the package re-exports, binds with the package as its barrel.
    #[test]
    fn test_import_member_python_explicit_member_binds_first_hit() {
        let facts = Imports {
            modules: vec![
                ("pkg.util", Module::Unique("pkg/util.py")),
                ("lib.extra", Module::Unique("lib/extra.py")),
                ("pkg", Module::Unique("pkg/__init__.py")),
            ],
            members: vec![
                ("pkg/util.py", "helper", MemberOwner::Any),
                ("lib/extra.py", "helper", MemberOwner::Any),
            ],
            reexports: vec![("pkg/__init__.py", "render", 7)],
            ..Imports::default()
        };
        let imports = [
            import("pkg.util", "helper", None),
            import("lib.extra", "helper", None),
            import("pkg", "render", None),
        ];
        let run = |name| {
            import_member(
                &facts,
                IMPORTER,
                Language::Python,
                name,
                &imports,
                CALLABLE,
                &[],
            )
        };
        assert_eq!(run("helper"), ImportMember::Bound(hit(0, &imports[0])));
        assert_eq!(
            run("render"),
            ImportMember::Bound(ImportHit {
                id: 7,
                import: &imports[2],
                barrel: Some("pkg/__init__.py"),
            })
        );
    }

    /// An import of a name its module does not declare: a module that may be
    /// local keeps the general tiers (Python under the callee, a fully
    /// qualified language with no retry name); a module certainly external
    /// binds nothing.
    #[test]
    fn test_import_member_unexported_name_local_retries_external_stops() {
        let facts = Imports {
            modules: vec![
                ("pkg.util", Module::Unique("pkg/util.py")),
                ("lib.Util.absent", Module::Unique("lib/Util.java")),
            ],
            module_names: vec!["pkg", "lib"],
            ..Imports::default()
        };
        let python = [
            import("pkg.util", "missing", None),
            import("requests", "get", None),
        ];
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Python,
                "missing",
                &python,
                CALLABLE,
                &[]
            ),
            ImportMember::Retry("missing")
        );
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Python,
                "get",
                &python,
                CALLABLE,
                &[]
            ),
            ImportMember::External
        );
        let java = [
            import("lib.Util.absent", "absent", Some("absent")),
            import("com.ext.Net.fetch", "fetch", Some("fetch")),
        ];
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Java,
                "absent",
                &java,
                CALLABLE,
                &[]
            ),
            ImportMember::Continue { wildcard: false }
        );
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Java,
                "fetch",
                &java,
                CALLABLE,
                &[]
            ),
            ImportMember::External
        );
    }

    /// Two explicit imports of one name holding different members are no
    /// hit in a fully qualified language: the general tiers decide.
    #[test]
    fn test_import_member_java_two_imports_one_name_continues() {
        let facts = Imports {
            modules: vec![
                ("lib.Util.run", Module::Unique("lib/Util.java")),
                ("lib.Other.run", Module::Unique("lib/Other.java")),
            ],
            members: vec![
                ("lib/Util.java", "run", MemberOwner::Type("lib.Util")),
                ("lib/Other.java", "run", MemberOwner::Type("lib.Other")),
            ],
            ..Imports::default()
        };
        let imports = [
            import("lib.Util.run", "run", Some("run")),
            import("lib.Other.run", "run", Some("run")),
        ];
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Java,
                "run",
                &imports,
                CALLABLE,
                &[]
            ),
            ImportMember::Continue { wildcard: false }
        );
        // One import alone binds its member.
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Java,
                "run",
                &imports[..1],
                CALLABLE,
                &[]
            ),
            ImportMember::Bound(hit(0, &imports[0]))
        );
    }

    /// A Kotlin call through an external class binding retries its bare
    /// member, unless the qualifier is the caller's base (`super.x()`).
    #[test]
    fn test_import_member_kotlin_external_class_binding_retries_extension() {
        let facts = Imports::default();
        let imports = [import(
            "kotlinx.serialization.protobuf.ProtoBuf",
            "ProtoBuf",
            None,
        )];
        let name = "ProtoBuf.asConverterFactory";
        let base = ["ProtoBuf".to_owned()];
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Kotlin,
                name,
                &imports,
                CALLABLE,
                &[]
            ),
            ImportMember::Extension("asConverterFactory")
        );
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Kotlin,
                name,
                &imports,
                CALLABLE,
                &base
            ),
            ImportMember::External
        );
        // Java has no extension functions.
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Java,
                name,
                &imports,
                CALLABLE,
                &[]
            ),
            ImportMember::External
        );
    }

    /// A star import never binds in the explicit pass: it opens the wildcard
    /// pass after heritage, which binds a top-level member of the package
    /// only. No import of the name, or none at all, leaves it closed.
    #[test]
    fn test_wildcard_member_star_import_binds_after_explicit_miss() {
        let facts = Imports {
            modules: vec![("util", Module::Namespace(vec!["util/Tools.kt"].into()))],
            members: vec![("util/Tools.kt", "tidy", MemberOwner::TopLevel)],
            ..Imports::default()
        };
        let imports = [import("base.Base", "Base", None), import("util", "*", None)];
        let open = match import_member(
            &facts,
            IMPORTER,
            Language::Kotlin,
            "tidy",
            &imports,
            CALLABLE,
            &[],
        ) {
            ImportMember::Continue { wildcard } => wildcard,
            other => panic!("tier settled the star import: {other:?}"),
        };
        assert!(open);
        let wildcard = |open, name| {
            wildcard_member(
                &facts,
                open,
                IMPORTER,
                Language::Kotlin,
                name,
                &imports,
                CALLABLE,
            )
        };
        assert_eq!(wildcard(true, "tidy"), Some(hit(0, &imports[1])));
        assert_eq!(wildcard(false, "tidy"), None);
        // A qualified name is no wildcard binding.
        assert_eq!(wildcard(true, "Tools.tidy"), None);
        assert_eq!(
            import_member(
                &facts,
                IMPORTER,
                Language::Kotlin,
                "tidy",
                &[],
                CALLABLE,
                &[]
            ),
            ImportMember::Continue { wildcard: false }
        );
        // An empty and a whitespace-only name: no import binds them.
        for name in ["", "  "] {
            assert_eq!(
                import_member(
                    &facts,
                    IMPORTER,
                    Language::Kotlin,
                    name,
                    &imports[..1],
                    CALLABLE,
                    &[]
                ),
                ImportMember::Continue { wildcard: false },
                "{name:?}"
            );
        }
        // The wildcard's module was removed: no file declares the package.
        let removed = Imports::default();
        assert_eq!(
            wildcard_member(
                &removed,
                true,
                IMPORTER,
                Language::Kotlin,
                "tidy",
                &imports,
                CALLABLE
            ),
            None
        );
    }

    /// The import-only path takes the wildcard pass only with `wildcards`:
    /// the index's `resolve_imported_symbol` does, the overlay's name
    /// reference import tier does not.
    #[test]
    fn test_imported_member_wildcards_flag_gates_wildcard_pass() {
        let facts = Imports {
            modules: vec![("app.Consts", Module::Unique("app/Consts.java"))],
            members: vec![("app/Consts.java", "TABLE", MemberOwner::Type("app.Consts"))],
            ..Imports::default()
        };
        let imports = [import("app.Consts", "*", Some("static:*"))];
        let run = |wildcards| {
            imported_member(
                &facts,
                IMPORTER,
                Language::Java,
                "TABLE",
                &imports,
                ResolveTarget::Field,
                wildcards,
            )
        };
        assert_eq!(run(true), MemberHit::Bound(hit(0, &imports[0])));
        assert_eq!(run(false), MemberHit::Unbound { bound: false });
    }

    /// A relative import, an indexed head and a found module each may be
    /// local; only an absolute, unindexed, undiscovered one is external.
    #[test]
    fn test_import_is_external_needs_every_proof() {
        let facts = Imports {
            modules: vec![("lib.Found", Module::Unique("lib/Found.java"))],
            module_names: vec!["pkg"],
            ..Imports::default()
        };
        let external = |source: &str, name: &str| {
            import_is_external(
                &facts,
                IMPORTER,
                &import(source, name, None),
                Language::Java,
            )
        };
        assert!(external("com.ext.Net", "Net"));
        assert!(!external("pkg.Missing", "Missing"));
        assert!(!external("lib.Found", "Found"));
        // A single segment and a wildcard never suppress.
        assert!(!external("Helper", "Helper"));
        assert!(!external("com.ext", "*"));
        let python = |source: &str| {
            import_is_external(
                &facts,
                IMPORTER,
                &import(source, "x", None),
                Language::Python,
            )
        };
        assert!(python("requests"));
        assert!(!python(".sibling"));
    }

    /// R17: only a Kotlin `Type.member` through a class binding retries, and
    /// never on the caller's base; the retry keeps top-level functions only.
    #[test]
    fn test_kotlin_extension_retry_class_binding_keeps_top_level_only() {
        let imports = [import("lib.Box", "Box", None)];
        let base = ["Box".to_owned()];
        let retry = |language, heritage: &[String]| {
            kotlin_extension_retry(language, "Box.grow", ("Box", "grow"), &imports, heritage)
        };
        assert_eq!(retry(Language::Kotlin, &[]), Some("grow"));
        assert_eq!(retry(Language::Kotlin, &base), None);
        assert_eq!(retry(Language::Java, &[]), None);
        assert_eq!(
            kotlin_extension_retry(Language::Kotlin, "Box.grow", ("Box", "grow"), &[], &[]),
            None
        );
        let decls: [Decl; 2] = [
            ("lib/Box.kt", "grow", M, Some("Box")),
            ("lib/BoxExt.kt", "grow", F, None),
        ];
        for facts in both(&decls) {
            let asked = Cell::new(0);
            let kept: Vec<_> = extension_fallback(&facts, "grow", |member| {
                assert_eq!(member, "grow");
                asked.set(asked.get() + 1);
                vec![(0, 0.85), (1, 0.7)]
            })
            .collect();
            assert_eq!(kept, [(1u32, 0.7f32)]);
            assert_eq!(asked.get(), 1);
            assert_eq!(
                extension_fallback(&facts, "grow", |_| None::<(u32, f32)>).next(),
                None
            );
        }
    }

    /// [`QualifierFacts`] over a [`Fake`]: no import binds, every Rust path
    /// agrees, and a type name denotes the types the fake declares under it.
    struct Quals {
        fake: Fake,
        files: Vec<&'static str>,
    }

    impl Quals {
        fn new(decls: &[Decl]) -> Self {
            let mut files: Vec<&'static str> = decls.iter().map(|decl| decl.0).collect();
            files.sort_unstable();
            files.dedup();
            Self {
                fake: Fake::new(Layout::Index, decls),
                files,
            }
        }
    }

    impl ImportModules for Quals {
        type Module = &'static str;

        fn import_module(
            &self,
            _importer: &str,
            _specifier: &str,
            _language: Language,
        ) -> Module<&'static str> {
            Module::Missing
        }

        fn module_name_indexed(&self, _language: Language, _name: &str) -> bool {
            false
        }
    }

    impl QualifierFacts for Quals {
        type Decls = Fake;

        fn decls(&self) -> &Fake {
            &self.fake
        }
        fn caller_declares(&self, source: &str, name: &str) -> bool {
            let wanted = ResolveTarget::Qualifier.kind_predicate();
            self.fake
                .in_file(source, name)
                .into_iter()
                .any(|id| wanted(self.fake.kind(id)))
        }
        fn stored_file(&self, path: &str) -> Option<&'static str> {
            self.files.iter().copied().find(|&file| file == path)
        }
        fn global_qualifier(&self, qualifier: &str, caller: FileMeta) -> GlobalPick {
            let wanted = ResolveTarget::Qualifier.kind_predicate();
            pick_global(
                caller,
                self.fake
                    .named(qualifier)
                    .into_iter()
                    .filter(|&(id, _)| wanted(self.fake.kind(id))),
            )
        }
        fn for_each_candidate(
            &self,
            _importer: &str,
            _specifier: &str,
            _visit: impl FnMut(&str) -> bool,
        ) {
        }
        fn exports(&self, _file: &str, _name: &str, _target: ResolveTarget) -> bool {
            false
        }
        fn holds_import(&self, _file: &str, _imported: &str, _default_import: bool) -> bool {
            false
        }
        fn rust_module_file(&self, _source: &str, _module_path: &str) -> Option<&'static str> {
            None
        }
        fn crate_module(
            &self,
            _source: &str,
            _module_path: &str,
            _member: &str,
            _target: ResolveTarget,
        ) -> CrateModule<&'static str> {
            CrateModule::Local
        }
        fn rust_path_agrees(
            &self,
            _source: &str,
            _callee: &str,
            _qualifier: &str,
            _candidate: &str,
            _imports: &[RawImport],
        ) -> Option<bool> {
            Some(true)
        }
        fn files_by_stem(&self, stem: &str) -> impl Iterator<Item = &str> {
            self.files.iter().copied().filter(move |&file| {
                std::path::Path::new(file)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    == Some(stem)
            })
        }
        fn type_candidates<'q>(
            &self,
            _source: &str,
            name: &'q str,
            _imports: &'q [RawImport],
        ) -> TypeCandidates<'q> {
            match self.fake.types_named(name).as_slice() {
                [] => TypeCandidates::None,
                &[id] => TypeCandidates::Unique(id, name),
                ids => TypeCandidates::Set(ids.to_vec(), name),
            }
        }
    }

    fn site<'s>(source: &'s str, callee: &'s str, call: Option<CallPick<'s>>) -> QualifiedSite<'s> {
        let (qualifier, member) = split_qualifier(callee).expect("a qualified callee");
        let caller = FileMeta::from_path(source);
        QualifiedSite {
            source,
            caller,
            language: caller.language,
            imports: &[],
            callee,
            qualifier,
            member,
            target: CALLABLE,
            call,
        }
    }

    /// [`qualified`] at `site` with the module tree answering `tree`; the
    /// count says how often the tree was asked.
    fn qualify(
        facts: &Quals,
        site: QualifiedSite<'_>,
        heritage: &[String],
        stem: StemFile,
        tree: Option<u32>,
    ) -> (Resolution, usize) {
        let asked = Cell::new(0);
        let (resolution, base) = qualified(facts, site, heritage, stem, || {
            asked.set(asked.get() + 1);
            tree
        });
        assert_eq!(base, None, "only super() names a base");
        (resolution, asked.get())
    }

    /// R13 by step: the caller's own file, a unique definition, a file
    /// named after the qualifier. A qualifier two files define, one no file
    /// defines, or a local variable binds nothing and leaves the call to the
    /// receiver ladder (`Unknown`); a `.` callee never asks the module tree.
    #[test]
    fn test_ladder_qualified_each_qualifier_step_returns_member_or_unknown() {
        let decls: [Decl; 10] = [
            ("app/Main.java", "Main", C, None),
            ("app/Main.java", "run", M, Some("Main")),
            ("app/K.java", "K", C, None),
            ("app/K.java", "save", M, Some("K")),
            ("app/Dup.java", "Dup", C, None),
            ("app/Dup.java", "go", M, Some("Dup")),
            ("lib/Dup.java", "Dup", C, None),
            ("lib/Dup.java", "go", M, Some("Dup")),
            ("app/util.py", "helper", F, None),
            ("app/main.py", "main", F, None),
        ];
        let facts = Quals::new(&decls);
        let cases: [(&str, &str, Resolution); 6] = [
            ("app/Main.java", "Main.run", bound(1, Tier::QualifierScoped)),
            ("app/Main.java", "K.save", bound(3, Tier::QualifierScoped)),
            (
                "app/main.py",
                "util.helper",
                bound(8, Tier::QualifierScoped),
            ),
            ("app/Main.java", "Dup.go", Resolution::Unknown),
            ("app/Main.java", "Gone.save", Resolution::Unknown),
            ("app/Main.java", "local.save", Resolution::Unknown),
        ];
        for (source, callee, want) in cases {
            for stem in [StemFile::Any, StemFile::DeclaringMember] {
                let got = qualify(&facts, site(source, callee, None), &[], stem, Some(99));
                assert_eq!(got, (want, 0), "{callee} from {source}, {stem:?}");
            }
        }
    }

    /// Divergence D1: the file named after the qualifier lacks the member.
    /// The index (`Any`) takes that file and binds nothing; the overlay
    /// (`DeclaringMember`) falls back to the caller's own file, where the
    /// caller type's member binds.
    #[test]
    fn test_ladder_qualified_stem_file_without_member_differs_by_parameter() {
        let decls: [Decl; 2] = [
            ("app/Foo.java", "Foo", C, None),
            ("app/FooExt.java", "a", M, Some("Foo")),
        ];
        let facts = Quals::new(&decls);
        let call = CallPick {
            owner: Some("Foo"),
            ..CallPick::default()
        };
        let at = site("app/FooExt.java", "Foo.a", Some(call));
        assert_eq!(
            qualify(&facts, at, &[], StemFile::Any, None),
            (Resolution::Unknown, 0)
        );
        assert_eq!(
            qualify(&facts, at, &[], StemFile::DeclaringMember, None),
            (bound(1, Tier::QualifierScoped), 0)
        );
    }

    /// Row 4 and R14: a Rust module path (`crate::x::parse`) never binds a
    /// method its qualifier does not own, at the qualifier tier or from the
    /// module tree, which is asked next; `K::st` and a module's free
    /// function bind at the qualifier tier and never ask it. A path whose
    /// module file is gone binds only what the module tree places.
    #[test]
    fn test_ladder_qualifier_rust_module_path_never_binds_method() {
        let decls: [Decl; 6] = [
            ("src/x.rs", "P", NodeKind::Struct, None),
            ("src/x.rs", "parse", M, Some("P")),
            ("src/x.rs", "helper", F, None),
            ("src/lib.rs", "x", NodeKind::Module, None),
            ("src/k.rs", "K", NodeKind::Struct, None),
            ("src/k.rs", "st", M, Some("K")),
        ];
        let facts = Quals::new(&decls);
        let ask = |facts: &Quals, callee, tree| {
            qualify(
                facts,
                site("src/lib.rs", callee, None),
                &[],
                StemFile::Any,
                tree,
            )
        };
        assert_eq!(
            ask(&facts, "crate::x::parse", None),
            (Resolution::Unknown, 1)
        );
        assert_eq!(
            ask(&facts, "crate::x::parse", Some(2)),
            (bound(2, Tier::ModuleTree), 1)
        );
        // The module tree's answer obeys the same rule: `P::parse` is no
        // member of the module `x`.
        assert_eq!(
            ask(&facts, "crate::x::parse", Some(1)),
            (Resolution::Unknown, 1)
        );
        assert_eq!(
            ask(&facts, "crate::x::helper", Some(2)),
            (bound(2, Tier::QualifierScoped), 0)
        );
        assert_eq!(
            ask(&facts, "K::st", Some(2)),
            (bound(5, Tier::QualifierScoped), 0)
        );
        assert_eq!(
            ask(&facts, "crate::gone::run", None),
            (Resolution::Unknown, 1)
        );
        // The same `::` shape outside Rust is no module path.
        let cpp: [Decl; 2] = [
            ("src/x.cpp", "P", C, None),
            ("src/x.cpp", "parse", M, Some("P")),
        ];
        let facts = Quals::new(&cpp);
        let at = site("src/main.cpp", "x::parse", None);
        let (resolution, asked) = qualify(&facts, at, &[], StemFile::Any, None);
        assert_eq!((resolution, asked), (bound(1, Tier::QualifierScoped), 0));
    }

    /// The prefix before the qualifier is internal only when it is empty or
    /// a `crate` / `self` / `super` chain.
    #[test]
    fn test_qualifier_prefix_is_internal_anchor_chain_returns_true() {
        let cases = [
            ("crate::x::parse", "x", true),
            ("self::x::parse", "x", true),
            ("super::super::x::parse", "x", true),
            ("x::parse", "x", true),
            ("std::fs::read", "fs", false),
            ("a::x::parse", "x", false),
            ("parse", "x", false),
        ];
        for (callee, qualifier, want) in cases {
            assert_eq!(
                qualifier_prefix_is_internal(callee, qualifier),
                want,
                "{callee}"
            );
        }
    }

    /// R12: Python `super().m()` walks the caller's bases by owner. The
    /// first base owning `m` binds (`TypeOwned`, with the base named); one
    /// that only inherits it binds at `TypeHeritage` unless a later base
    /// owns another `m`. No base, an unknown base, or a call outside a
    /// class gives no edge, and the module tree is never asked.
    #[test]
    fn test_ladder_super_member_python_bases_returns_each_verdict() {
        let decls: [Decl; 7] = [
            ("app/base.py", "Base", C, None),
            ("app/base.py", "m", M, Some("Base")),
            ("app/other.py", "Other", C, None),
            ("app/other.py", "m", M, Some("Other")),
            ("app/mid.py", "Mid", C, None),
            ("app/plain.py", "Plain", C, None),
            ("lib/m.py", "m", F, None),
        ];
        let mut facts = Quals::new(&decls);
        facts.fake.supertypes = vec![(0, vec![]), (2, vec![]), (4, vec![0]), (5, vec![])];
        let names = |bases: &[&str]| bases.iter().map(|&b| b.to_owned()).collect::<Vec<_>>();
        let in_class = CallPick {
            owner: Some("Sub"),
            ..CallPick::default()
        };
        type SuperCase<'a> = (
            &'a [&'a str],
            Option<CallPick<'a>>,
            Resolution,
            Option<&'a str>,
        );
        let cases: [SuperCase<'_>; 8] = [
            (
                &["Base"],
                Some(in_class),
                bound(1, Tier::TypeOwned),
                Some("Base"),
            ),
            (
                &["Other", "Base"],
                Some(in_class),
                bound(3, Tier::TypeOwned),
                Some("Other"),
            ),
            (
                &["Plain", "Base"],
                Some(in_class),
                bound(1, Tier::TypeOwned),
                Some("Base"),
            ),
            (
                &["Mid"],
                Some(in_class),
                bound(1, Tier::TypeHeritage),
                Some("Mid"),
            ),
            (
                &["Mid", "Other"],
                Some(in_class),
                Resolution::NoEdge(NoEdge::Unresolved),
                None,
            ),
            (
                &["Gone", "Base"],
                Some(in_class),
                Resolution::NoEdge(NoEdge::Unresolved),
                None,
            ),
            (
                &[],
                Some(in_class),
                Resolution::NoEdge(NoEdge::Unresolved),
                None,
            ),
            (&[], None, Resolution::NoEdge(NoEdge::Unresolved), None),
        ];
        for (bases, call, want, base) in cases {
            let heritage = names(bases);
            let at = site("app/sub.py", "super().m", call);
            let got = qualified(&facts, at, &heritage, StemFile::Any, || {
                panic!("super() never asks the module tree")
            });
            assert_eq!(got, (want, base), "bases {bases:?}");
        }
        // `super()` outside Python is an ordinary qualifier.
        let at = site("app/Sub.java", "super().m", None);
        assert_eq!(
            qualify(&facts, at, &names(&["Base"]), StemFile::Any, None),
            (Resolution::Unknown, 0)
        );
    }

    /// One `pub use` row of a [`Tree`]: `(module file, published name,
    /// prefix, original, containing module path)`; a glob is named `*`.
    type ReExportRow = (&'static str, &'static str, Vec<String>, String, Vec<String>);

    /// [`ModuleTreeFacts`] over fixed rows. A root, a module and a file are
    /// all the module's file, as the overlay stores them.
    #[derive(Default)]
    struct Tree {
        /// `(code name, lib entry file)`, one row per crate table row.
        crates: Vec<(&'static str, &'static str)>,
        /// `(caller file, its root, its module path)`.
        callers: Vec<(&'static str, &'static str, &'static [&'static str])>,
        /// `(root, module path, file)` for each file-backed `mod`.
        modules: Vec<(&'static str, &'static [&'static str], &'static str)>,
        reexports: Vec<ReExportRow>,
        /// `(module file, name)` a `use` of unknown visibility names.
        unknown: Vec<(&'static str, &'static str)>,
    }

    impl ModuleTreeFacts for Tree {
        type Root = &'static str;
        type Module = &'static str;
        type File = &'static str;
        type Target = &'static str;

        fn crate_root(&self, name: &str) -> Option<&'static str> {
            let mut rows = self.crates.iter().filter(|row| row.0 == name);
            match (rows.next(), rows.next()) {
                (Some(&(_, root)), None) => Some(root),
                _ => None,
            }
        }
        fn caller_crate(&self, caller: &str) -> Option<&'static str> {
            self.callers
                .iter()
                .find(|row| row.0 == caller)
                .map(|row| row.1)
        }
        fn caller_module(&self, caller: &str) -> Option<(&'static str, Vec<String>)> {
            self.callers
                .iter()
                .find(|row| row.0 == caller)
                .map(|&(_, root, path)| (root, path.iter().map(|&s| s.to_owned()).collect()))
        }
        fn root_module(&self, root: &'static str) -> Option<&'static str> {
            Some(root)
        }
        fn submodule(
            &self,
            root: &'static str,
            _parent: &'static str,
            path: &[String],
        ) -> Option<&'static str> {
            self.modules
                .iter()
                .find(|row| {
                    row.0 == root
                        && row.1.len() == path.len()
                        && row.1.iter().zip(path).all(|(&a, b)| a == b.as_str())
                })
                .map(|row| row.2)
        }
        fn published(
            &self,
            _root: &'static str,
            module: &'static str,
            name: &str,
        ) -> Published<'_> {
            if self.unknown.iter().any(|&(m, n)| m == module && n == name) {
                return Published::Unknown;
            }
            let row = |wanted: &str| {
                self.reexports
                    .iter()
                    .find(|row| row.0 == module && row.1 == wanted)
            };
            match row(name).or_else(|| row("*")) {
                Some(row) => Published::ReExport(ReExport {
                    prefix: &row.2,
                    original: &row.3,
                    containing: &row.4,
                }),
                None => Published::Own,
            }
        }
        fn module_file(&self, module: &'static str) -> Option<&'static str> {
            Some(module)
        }
        fn declaring_targets(&self, caller: &str) -> Vec<(&'static str, Vec<String>)> {
            self.callers
                .iter()
                .filter(|row| row.0 == caller)
                .map(|&(_, root, path)| (root, path.iter().map(|&s| s.to_owned()).collect()))
                .collect()
        }
        fn target_module(&self, target: &'static str, path: &[String]) -> Option<&'static str> {
            if path.is_empty() {
                return Some(target);
            }
            self.submodule(target, target, path)
        }
    }

    fn segments(path: &[&str]) -> Vec<String> {
        path.iter().map(|&s| s.to_owned()).collect()
    }

    const A_LIB: &str = "crates/a/src/lib.rs";
    const B_LIB: &str = "crates/b/src/lib.rs";
    const B_M: &str = "crates/b/src/m/mod.rs";
    const B_DEEP: &str = "crates/b/src/m/deep.rs";

    /// Two crates: `a` (`mod x;`, `pub use b::m::f;`) and `b` (`mod m;` as
    /// `m/mod.rs` with `mod deep;`, `mod inner;`, `pub use inner::made as
    /// make;`, `pub use inner::*;`). `x::hidden` is a `use` whose
    /// visibility the side cannot see.
    fn workspace() -> (Tree, Vec<Decl>) {
        let tree = Tree {
            crates: vec![("a", A_LIB), ("b", B_LIB)],
            callers: vec![
                (A_LIB, A_LIB, &[]),
                ("crates/a/src/x.rs", A_LIB, &["x"]),
                (B_DEEP, B_LIB, &["m", "deep"]),
            ],
            modules: vec![
                (A_LIB, &["x"], "crates/a/src/x.rs"),
                (B_LIB, &["m"], B_M),
                (B_LIB, &["m", "deep"], B_DEEP),
                (B_LIB, &["inner"], "crates/b/src/inner.rs"),
            ],
            reexports: vec![
                (A_LIB, "f", segments(&["b", "m"]), "f".into(), vec![]),
                (B_LIB, "make", segments(&["inner"]), "made".into(), vec![]),
                (B_LIB, "*", segments(&["inner"]), "*".into(), vec![]),
            ],
            unknown: vec![("crates/a/src/x.rs", "hidden")],
        };
        let decls = vec![
            (B_M, "f", F, None),
            (B_DEEP, "g", F, None),
            ("crates/b/src/inner.rs", "made", F, None),
            ("crates/b/src/inner.rs", "globbed", F, None),
            ("crates/a/src/x.rs", "P", NodeKind::Struct, None),
            ("crates/a/src/x.rs", "parse", M, Some("P")),
            ("crates/a/src/x.rs", "helper", F, None),
            (A_LIB, "x", NodeKind::Module, None),
            (A_LIB, "gone", NodeKind::Module, None),
            ("crates/a/src/x.rs", "hidden", F, None),
        ];
        (tree, decls)
    }

    fn member(tree: &Tree, decls: &Fake, caller: &str, callee: &str) -> Option<u32> {
        let (_, last) = callee.rsplit_once("::").expect("a path");
        module_tree_member(tree, decls, caller, callee, last, CALLABLE)
    }

    /// R14: a head no crate table row names binds nothing, and neither do
    /// `std` and the other external crates; a named crate binds.
    #[test]
    fn test_module_tree_member_crate_name_absent_from_table_returns_none() {
        let (tree, decls) = workspace();
        let fake = Fake::new(Layout::Index, &decls);
        assert_eq!(member(&tree, &fake, A_LIB, "b::m::f"), Some(0));
        for callee in ["nope::m::f", "std::m::f", "core::m::f"] {
            assert_eq!(member(&tree, &fake, A_LIB, callee), None, "{callee}");
            assert_eq!(module_tree_item(&tree, A_LIB, callee), None, "{callee}");
        }
        assert_eq!(module_tree_item(&tree, A_LIB, "f"), None);
    }

    /// R14: the table gives no crate for a name two of its rows carry, so
    /// the head binds no tree: no edge, never a guess between them, and a
    /// `use` of it stays local.
    #[test]
    fn test_module_tree_member_two_crates_one_name_returns_none() {
        let (mut tree, decls) = workspace();
        tree.crates.push(("b", "vendor/b/src/lib.rs"));
        tree.modules
            .push(("vendor/b/src/lib.rs", &["m"], "vendor/b/src/m.rs"));
        let decls: Vec<Decl> = decls
            .into_iter()
            .chain([("vendor/b/src/m.rs", "f", F, None)])
            .collect();
        let fake = Fake::new(Layout::Index, &decls);
        assert_eq!(member(&tree, &fake, A_LIB, "b::m::f"), None);
        assert_eq!(
            crate_module(&tree, &fake, A_LIB, "b::m", "f", CALLABLE),
            CrateModule::Local
        );
        // `a`'s `pub use b::m::f` leads nowhere either: `crate::f` stays in
        // `a`'s root, which declares no `f`.
        assert_eq!(member(&tree, &fake, A_LIB, "crate::f"), None);
    }

    /// R14: the `pub use` chain places the item at its definition: a
    /// renamed re-export (`make` → `inner::made`), a glob, and a re-export
    /// of another crate's item. A `use` whose visibility the side cannot
    /// see binds nothing, even when the module declares the name.
    #[test]
    fn test_module_tree_member_pub_use_chain_binds_defining_file() {
        let (tree, decls) = workspace();
        let fake = Fake::new(Layout::Index, &decls);
        assert_eq!(member(&tree, &fake, A_LIB, "b::make"), Some(2));
        assert_eq!(member(&tree, &fake, A_LIB, "b::globbed"), Some(3));
        assert_eq!(member(&tree, &fake, A_LIB, "crate::f"), Some(0));
        assert_eq!(
            module_tree_item(&tree, A_LIB, "b::make"),
            Some(TreeItem::Found {
                file: "crates/b/src/inner.rs",
                item: "made".into()
            })
        );
        assert_eq!(
            module_tree_item(&tree, A_LIB, "crate::x::hidden"),
            Some(TreeItem::Unknown)
        );
        assert_eq!(member(&tree, &fake, A_LIB, "crate::x::hidden"), None);
    }

    /// R14: a module whose file is a directory's `mod.rs` holds its items
    /// and roots its own submodules; `self::` and `super::` start from the
    /// caller's module.
    #[test]
    fn test_module_tree_member_directory_mod_rs_module_binds_its_file() {
        let (tree, decls) = workspace();
        let fake = Fake::new(Layout::Index, &decls);
        let cases = [
            (A_LIB, "b::m::f", Some(0)),
            (A_LIB, "b::m::deep::g", Some(1)),
            (B_DEEP, "super::f", Some(0)),
            (B_DEEP, "super::super::m::f", Some(0)),
            (B_DEEP, "self::g", Some(1)),
            (B_DEEP, "crate::m::deep::g", Some(1)),
            (B_DEEP, "super::super::super::f", None),
        ];
        for (caller, callee, want) in cases {
            assert_eq!(
                member(&tree, &fake, caller, callee),
                want,
                "{callee} from {caller}"
            );
        }
        assert_eq!(module_tree_module(&tree, A_LIB, "b::m"), Some(B_M));
    }

    /// R14: a module whose file is removed is no module. The path stops at
    /// the declaring file, which declares no `run`, so nothing binds, and
    /// a `use` of the module stays local.
    #[test]
    fn test_module_tree_member_removed_module_file_returns_none() {
        let (tree, decls) = workspace();
        let fake = Fake::new(Layout::Index, &decls);
        assert_eq!(member(&tree, &fake, A_LIB, "crate::gone::run"), None);
        assert_eq!(module_tree_module(&tree, A_LIB, "crate::gone"), None);
        assert_eq!(
            crate_module(&tree, &fake, A_LIB, "a::gone", "run", CALLABLE),
            CrateModule::Local
        );
    }

    const LIB: &str = "src/lib.rs";
    const MAIN: &str = "src/main.rs";
    const SHARED: &str = "src/shared.rs";

    /// A file `shared.rs` that each given root declares as `mod shared;`,
    /// and the `x` module of each root at its own file.
    fn targets(roots: &[&'static str]) -> Tree {
        let mut tree = Tree::default();
        for &root in roots {
            tree.callers.push((SHARED, root, &["shared"]));
            tree.modules.push((root, &["shared"], SHARED));
            tree.modules.push((
                root,
                &["x"],
                if root == LIB {
                    "src/x_lib.rs"
                } else {
                    "src/x_main.rs"
                },
            ));
        }
        tree
    }

    #[test]
    fn test_anchored_module_file_one_target_names_its_file() {
        let tree = targets(&[LIB]);
        assert_eq!(
            anchored_module_file(&tree, SHARED, "crate::x"),
            Some("src/x_lib.rs")
        );
        assert_eq!(anchored_module_file(&tree, SHARED, "crate"), Some(LIB));
        assert_eq!(anchored_module_file(&tree, SHARED, "self"), Some(SHARED));
        assert_eq!(anchored_module_file(&tree, SHARED, "super"), Some(LIB));
    }

    #[test]
    fn test_anchored_module_file_targets_agree_names_the_shared_file() {
        let tree = targets(&[LIB, MAIN]);
        assert_eq!(anchored_module_file(&tree, SHARED, "self"), Some(SHARED));
    }

    #[test]
    fn test_anchored_module_file_targets_disagree_returns_none() {
        let tree = targets(&[LIB, MAIN]);
        assert_eq!(anchored_module_file(&tree, SHARED, "crate::x"), None);
        assert_eq!(anchored_module_file(&tree, SHARED, "crate"), None);
        assert_eq!(anchored_module_file(&tree, SHARED, "super"), None);
    }

    #[test]
    fn test_anchored_module_file_one_target_misses_the_module_returns_none() {
        let mut tree = targets(&[LIB, MAIN]);
        tree.modules
            .retain(|row| !(row.0 == MAIN && row.1 == ["x"]));
        assert_eq!(anchored_module_file(&tree, SHARED, "crate::x"), None);
    }

    #[test]
    fn test_anchored_module_file_no_declaring_target_returns_none() {
        let tree = targets(&[LIB]);
        assert_eq!(anchored_module_file(&tree, "src/orphan.rs", "crate"), None);
    }

    #[test]
    fn test_anchored_module_file_unanchored_or_empty_path_returns_none() {
        let tree = targets(&[LIB]);
        for path in ["", " ", "::", "std::fs", "super::super", "crate::missing"] {
            assert_eq!(anchored_module_file(&tree, SHARED, path), None, "{path:?}");
        }
    }

    /// Row 4 through the real tree: the module tree places `parse`, a
    /// method of `P`, in `x.rs`, and [`qualified`] still binds nothing,
    /// since the module `x` does not own it.
    #[test]
    fn test_module_tree_member_method_last_segment_never_binds() {
        let (tree, decls) = workspace();
        let fake = Fake::new(Layout::Index, &decls);
        let placed = member(&tree, &fake, A_LIB, "crate::x::parse");
        assert_eq!(placed, Some(5));
        let facts = Quals::new(&decls);
        let at = site(A_LIB, "crate::x::parse", None);
        assert_eq!(
            qualify(&facts, at, &[], StemFile::Any, placed),
            (Resolution::Unknown, 1)
        );
    }

    /// Divergence D2: a `use` head that names a workspace crate places the
    /// module's file, as the index does; `crate` / `self` / `super` heads
    /// and unknown crates stay local, and a module that lacks the member
    /// places no file.
    #[test]
    fn test_crate_module_workspace_crate_head_returns_placed_file() {
        let (tree, decls) = workspace();
        let fake = Fake::new(Layout::Index, &decls);
        let cases = [
            ("b::m", "f", CrateModule::Placed(Some(B_M))),
            ("b::m::deep", "g", CrateModule::Placed(Some(B_DEEP))),
            ("b::m", "absent", CrateModule::Placed(None)),
            ("crate::x", "helper", CrateModule::Local),
            ("nope::m", "f", CrateModule::Local),
        ];
        for (module_path, name, want) in cases {
            assert_eq!(
                crate_module(&tree, &fake, A_LIB, module_path, name, CALLABLE),
                want,
                "{module_path}::{name}"
            );
        }
    }

    /// [`CallFacts`] over [`Fake`]: what the ladder and each lookup bind, by
    /// name, and a log of what [`call`] asked.
    struct CallFake {
        decls: Fake,
        /// The ladder's verdict per lookup name; any other name is `Unknown`.
        ladder: Vec<(&'static str, Resolution)>,
        /// What the bare-member lookup binds, per member.
        bare: Vec<(&'static str, (u32, f32))>,
        /// What the type lookup binds, per type path.
        types: Vec<(&'static str, (u32, f32))>,
        /// The receivers that name a type.
        typed_receivers: &'static [&'static str],
        /// The names a constructible type and an admitted method share.
        receiverless: &'static [&'static str],
        /// `(type, its one constructor)`.
        constructors: Vec<(u32, u32)>,
        /// The targets the C# access rule refuses.
        refused: Vec<u32>,
        /// Each ladder call: the lookup, `untyped`, `free_only`, and how
        /// many bases it received.
        seen: RefCell<Vec<(String, bool, bool, usize)>>,
        bare_calls: Cell<u32>,
        unresolved: RefCell<Vec<String>>,
    }

    fn call_fake(decls: &[Decl]) -> CallFake {
        CallFake {
            decls: Fake::new(Layout::Index, decls),
            ladder: Vec::new(),
            bare: Vec::new(),
            types: Vec::new(),
            typed_receivers: &[],
            receiverless: &[],
            constructors: Vec::new(),
            refused: Vec::new(),
            seen: RefCell::default(),
            bare_calls: Cell::new(0),
            unresolved: RefCell::default(),
        }
    }

    /// No module facts: the sites these tests plant carry no imports.
    impl ModuleFacts for CallFake {
        type Path = String;
        fn has_file(&self, _file: &str) -> bool {
            false
        }
        fn has_module_name(&self, _language: Language, _name: &str) -> bool {
            false
        }
        fn declares(&self, _file: &str, _name: &str) -> bool {
            false
        }
        fn decls_in_file(&self, _file: &str, _name: &str) -> Vec<(u32, NodeKind)> {
            Vec::new()
        }
        fn is_exported(&self, _decl: u32) -> bool {
            false
        }
        fn is_es_module(&self, _file: &str) -> bool {
            false
        }
        fn export_aliases(&self, _file: &str) -> Option<Fact<'_, ExportAliasTable>> {
            None
        }
        fn imports(&self, _file: &str) -> Option<FileImports<'_>> {
            None
        }
        fn star_modules(&self, _module: &str) -> Fact<'_, [Box<str>]> {
            Fact::Borrowed(&[])
        }
        fn python_module(&self, _importer: &str, _specifier: &str) -> PyModule<String> {
            PyModule::Missing
        }
        fn for_each_candidate(
            &self,
            _importer: &str,
            _specifier: &str,
            _visit: &mut dyn FnMut(&str) -> bool,
        ) {
        }
    }

    impl<'s> CallFacts<'s> for CallFake {
        type Modules = CallFake;
        type Decls = Fake;
        const TYPE_NAME: TypeName = TypeName::Qualifier;

        fn modules(&self) -> &CallFake {
            self
        }
        fn decls(&self) -> &Fake {
            &self.decls
        }
        fn type_candidates<'q>(
            &self,
            _source: &str,
            receiver: &'q str,
            _imports: &'q [RawImport],
        ) -> TypeCandidates<'q> {
            match self.typed_receivers.contains(&receiver) {
                true => TypeCandidates::Unique(0, receiver),
                false => TypeCandidates::None,
            }
        }
        fn ladder(
            &self,
            site: CallSite<'s>,
            typed: Option<&str>,
            call: CallPick<'_>,
            heritage: &[String],
        ) -> Resolution {
            let lookup = typed.unwrap_or(site.name());
            self.seen.borrow_mut().push((
                lookup.to_owned(),
                call.untyped,
                call.free_only,
                heritage.len(),
            ));
            self.ladder
                .iter()
                .find(|(name, _)| *name == lookup)
                .map_or(Resolution::Unknown, |&(_, resolution)| resolution)
        }
        fn csharp_admitted(&self, id: u32, _site: CallSite<'_>) -> Option<u32> {
            (!self.refused.contains(&id)).then_some(id)
        }
        fn bare_member(
            &self,
            member: &str,
            _target: ResolveTarget,
            _heritage: &[String],
        ) -> Option<(u32, f32)> {
            self.bare_calls.set(self.bare_calls.get() + 1);
            self.bare
                .iter()
                .find(|(name, _)| *name == member)
                .map(|&(_, hit)| hit)
        }
        fn names_type_and_member(&self, callee: &str) -> bool {
            self.receiverless.contains(&callee)
        }
        fn declares_constructible(&self, name: &str) -> bool {
            self.decls
                .decls
                .iter()
                .any(|decl| decl.1 == name && decl.2.is_constructible())
        }
        fn resolve_type(
            &self,
            path: &str,
            _bind_imports: bool,
            _call: CallPick<'_>,
            _heritage: &[String],
        ) -> Option<(u32, f32)> {
            self.types
                .iter()
                .find(|(name, _)| *name == path)
                .map(|&(_, hit)| hit)
        }
        fn construction_target(&self, type_id: u32) -> u32 {
            self.constructors
                .iter()
                .find(|(ty, _)| *ty == type_id)
                .map_or(type_id, |&(_, ctor)| ctor)
        }
        fn note_untyped_unresolved(&self, name: &str) {
            self.unresolved.borrow_mut().push(name.to_owned());
        }
    }

    const CTOR: NodeKind = NodeKind::Constructor;

    fn cx<'c>(source: &'c str, language: Language, heritage: &'c [String]) -> CallContext<'c> {
        CallContext {
            source,
            language,
            imports: &[],
            heritage,
            owner: Some("Caller"),
            caller: Some(99),
        }
    }

    /// B §3.2 row `NoEdge(r)`, Ladder reach: a Kotlin typed receiver the
    /// ladder leaves external retries the bare member, which binds a
    /// top-level function only. Dart types receivers too but has no
    /// extension retry.
    #[test]
    fn test_call_kotlin_typed_receiver_external_retries_free_extension() {
        let mut fake = call_fake(&[
            ("lib/BoxExt.kt", "grow", F, None),
            ("lib/Box.kt", "grow", M, Some("Other")),
        ]);
        fake.typed_receivers = &["Box"];
        fake.ladder = vec![("Box.grow", Resolution::NoEdge(NoEdge::External))];
        fake.bare = vec![("grow", (0, 0.7))];
        let site = CallSite::FromReceiver {
            receiver: "Box",
            member: "grow",
        };
        let kotlin = call(&fake, cx("app/Main.kt", Language::Kotlin, &[]), site);
        assert_eq!(kotlin.resolution, Resolution::from_ladder(Some((0, 0.7))));
        assert!(kotlin.typed);
        assert_eq!(fake.seen.borrow()[0].0, "Box.grow");

        fake.bare = vec![("grow", (1, 0.7))];
        let method = call(&fake, cx("app/Main.kt", Language::Kotlin, &[]), site);
        assert_eq!(
            method.resolution,
            Resolution::NoEdge(NoEdge::Unresolved),
            "a member is no extension"
        );

        fake.bare_calls.set(0);
        let dart = call(&fake, cx("lib/main.dart", Language::Dart, &[]), site);
        assert_eq!(dart.resolution, Resolution::NoEdge(NoEdge::External));
        assert_eq!(fake.bare_calls.get(), 0);
    }

    /// B §3.2 row `NoEdge(r)`, Call reach: neither the Kotlin retry nor the
    /// constructor fallback runs, though each would bind.
    #[test]
    fn test_call_noedge_of_call_reach_skips_every_fallback() {
        let mut fake = call_fake(&[
            ("pkg/widget.py", "Widget", C, None),
            ("lib/BoxExt.kt", "grow", F, None),
        ]);
        fake.typed_receivers = &["Box"];
        fake.types = vec![("Widget", (0, 0.7))];
        fake.bare = vec![("grow", (1, 0.7))];
        fake.ladder = vec![
            ("Widget", Resolution::NoEdge(NoEdge::PackageMissing)),
            ("Box.grow", Resolution::NoEdge(NoEdge::DeclaredAttribute)),
        ];
        let python = call(
            &fake,
            cx("app/main.py", Language::Python, &[]),
            CallSite::Plain("Widget"),
        );
        assert_eq!(
            python.resolution,
            Resolution::NoEdge(NoEdge::PackageMissing)
        );
        let site = CallSite::FromReceiver {
            receiver: "Box",
            member: "grow",
        };
        let kotlin = call(&fake, cx("app/Main.kt", Language::Kotlin, &[]), site);
        assert_eq!(
            kotlin.resolution,
            Resolution::NoEdge(NoEdge::DeclaredAttribute)
        );
        assert_eq!(fake.bare_calls.get(), 0);
    }

    /// B §3.2: an ambiguous ladder leaves the constructor fallback, which
    /// lands on the class's one constructor at the type lookup's
    /// confidence. A bound call, a name a member shares, and an untyped
    /// member call each skip it.
    #[test]
    fn test_call_python_ambiguous_ladder_constructor_binds_sole_constructor() {
        let mut fake = call_fake(&[
            ("pkg/widget.py", "Widget", C, None),
            ("pkg/widget.py", "__init__", CTOR, Some("Widget")),
            ("pkg/a/make.py", "Widget", F, None),
        ]);
        fake.types = vec![("Widget", (0, 0.7))];
        fake.constructors = vec![(0, 1)];
        fake.ladder = vec![("Widget", Resolution::NoEdge(NoEdge::Ambiguous(2)))];
        let python = cx("app/main.py", Language::Python, &[]);
        let construct = call(&fake, python, CallSite::Plain("Widget"));
        assert_eq!(
            construct.resolution,
            Resolution::from_ladder(Some((1, 0.7)))
        );

        fake.receiverless = &["Widget"];
        let member = call(&fake, python, CallSite::Plain("Widget"));
        assert_eq!(member.resolution, Resolution::NoEdge(NoEdge::Ambiguous(2)));

        fake.receiverless = &[];
        fake.ladder = vec![("Widget", Resolution::bound(2, Tier::Global))];
        let function = call(&fake, python, CallSite::Plain("Widget"));
        assert_eq!(
            function.resolution,
            Resolution::bound(2, Tier::Global),
            "a class and a function of one name: the ladder's function stays"
        );

        fake.ladder = vec![("Widget", Resolution::NoEdge(NoEdge::Ambiguous(2)))];
        let untyped = call(&fake, python, CallSite::UntypedMember("Widget"));
        assert_eq!(untyped.resolution, Resolution::NoEdge(NoEdge::Ambiguous(2)));
    }

    /// R21: an untyped member call keeps a method of another type, and in
    /// Python another module's function; it refuses a function of the
    /// caller's module, a member of the caller's own type, and in Go any
    /// free function. Two classes' namesakes stay the ladder's no edge.
    #[test]
    fn test_call_untyped_member_post_filter_admits_member_or_other_module_function() {
        let mut fake = call_fake(&[
            ("app/main.py", "helper", F, None),
            ("lib/tools.py", "compute", F, None),
            ("lib/other.py", "save", M, Some("Other")),
            ("lib/caller.py", "own", M, Some("Caller")),
        ]);
        let python = cx("app/main.py", Language::Python, &[]);
        for (name, id, admitted) in [
            ("helper", 0, false),
            ("compute", 1, true),
            ("save", 2, true),
            ("own", 3, false),
        ] {
            fake.ladder = vec![(name, Resolution::bound(id, Tier::Global))];
            let verdict = call(&fake, python, CallSite::UntypedMember(name));
            let want = match admitted {
                true => Resolution::bound(id, Tier::Global),
                false => Resolution::Unknown,
            };
            assert_eq!(verdict.resolution, want, "{name}");
        }
        fake.ladder = vec![("compute", Resolution::bound(1, Tier::Global))];
        let go = call(
            &fake,
            cx("main.go", Language::Go, &[]),
            CallSite::UntypedMember("compute"),
        );
        assert_eq!(go.resolution, Resolution::Unknown);
        fake.ladder = vec![("save", Resolution::NoEdge(NoEdge::Ambiguous(2)))];
        let two = call(&fake, python, CallSite::UntypedMember("save"));
        assert_eq!(two.resolution, Resolution::NoEdge(NoEdge::Ambiguous(2)));
    }

    /// The overlay no longer sets the call pick by hand: [`call`] derives
    /// `untyped`, `free_only` and the bases from the normalized site. A
    /// Swift receiver that names no type is an untyped member; a typed one
    /// resolves as `Receiver.member`. A C# untyped member takes no tier.
    /// An empty callee reaches the ladder and binds nothing.
    #[test]
    fn test_call_pick_follows_normalized_site() {
        let heritage = vec!["Base".to_owned()];
        let mut fake = call_fake(&[]);
        fake.typed_receivers = &["Util"];
        let cases = [
            (
                Language::Python,
                CallSite::Plain("helper"),
                ("helper", false, true, 1),
            ),
            (
                Language::Python,
                CallSite::UntypedMember("m"),
                ("m", true, false, 0),
            ),
            (
                Language::Swift,
                CallSite::FromReceiver {
                    receiver: "value",
                    member: "make",
                },
                ("make", true, false, 0),
            ),
            (
                Language::Swift,
                CallSite::FromReceiver {
                    receiver: "Util",
                    member: "make",
                },
                ("Util.make", false, false, 1),
            ),
            (
                Language::Kotlin,
                CallSite::Plain("run"),
                ("run", false, false, 1),
            ),
            (Language::Python, CallSite::Plain(""), ("", false, true, 1)),
        ];
        for (language, site, (lookup, untyped, free_only, bases)) in cases {
            fake.seen.borrow_mut().clear();
            let verdict = call(&fake, cx("src/caller", language, &heritage), site);
            assert_eq!(verdict.resolution, Resolution::Unknown, "{site:?}");
            assert_eq!(
                fake.seen.borrow().as_slice(),
                [(lookup.to_owned(), untyped, free_only, bases)],
                "{language:?} {site:?}"
            );
        }
        fake.seen.borrow_mut().clear();
        let csharp = call(
            &fake,
            cx("src/Caller.cs", Language::CSharp, &heritage),
            CallSite::FromReceiver {
                receiver: "value",
                member: "Run",
            },
        );
        assert_eq!(csharp.resolution, Resolution::Unknown);
        assert_eq!(csharp.site, CallSite::UntypedMember("Run"));
        assert!(
            fake.seen.borrow().is_empty(),
            "no tier for a C# untyped member"
        );
        assert_eq!(fake.unresolved.borrow().as_slice(), ["Run".to_owned()]);
    }

    /// B §3.2 row `Bound`: a C# target the access rule refuses leaves the
    /// call `Unknown`, so the constructor fallback still binds `new
    /// Widget()`; an admitted target keeps the ladder's edge.
    #[test]
    fn test_call_csharp_access_refusal_lets_constructor_bind() {
        let mut fake = call_fake(&[
            ("src/Widget.cs", "Widget", C, None),
            ("src/Widget.cs", "Widget", CTOR, Some("Widget")),
            ("src/Other.cs", "Widget", M, Some("Other")),
        ]);
        fake.types = vec![("Widget", (0, 0.85))];
        fake.constructors = vec![(0, 1)];
        fake.ladder = vec![("Widget", Resolution::bound(2, Tier::Global))];
        let csharp = cx("src/App.cs", Language::CSharp, &[]);
        let admitted = call(&fake, csharp, CallSite::Construct("Widget"));
        assert_eq!(admitted.resolution, Resolution::bound(2, Tier::Global));
        fake.refused = vec![2];
        let refused = call(&fake, csharp, CallSite::Construct("Widget"));
        assert_eq!(refused.resolution, Resolution::from_ladder(Some((1, 0.85))));
    }

    /// An empty or whitespace-only callee, which a broken parse can leave,
    /// binds nothing on any path: the ladder misses it and no type passes
    /// the constructor gate under that name. Two identical calls on the same
    /// facts give one verdict, and each runs the ladder once: the entry
    /// keeps no state between sites.
    #[test]
    fn test_call_empty_callee_and_repeated_call_bind_nothing_twice() {
        let mut fake = call_fake(&[("pkg/widget.py", "Widget", C, None)]);
        fake.types = vec![("", (0, 0.7)), (" ", (0, 0.7))];
        for language in [Language::Python, Language::Kotlin, Language::Cpp] {
            for callee in ["", " "] {
                let site = CallSite::Plain(callee);
                let first = call(&fake, cx("app/main.py", language, &[]), site);
                let again = call(&fake, cx("app/main.py", language, &[]), site);
                assert_eq!(first.resolution.edge(), None, "{language:?} {callee:?}");
                assert_eq!(first.resolution, again.resolution);
            }
        }
        let empty = CallSite::FromReceiver {
            receiver: "",
            member: "",
        };
        let kotlin = call(&fake, cx("app/Main.kt", Language::Kotlin, &[]), empty);
        assert_eq!(kotlin.resolution.edge(), None);
        assert!(!kotlin.typed);
        let seen = fake.seen.borrow();
        assert_eq!(seen.len(), 13);
        assert!(seen.iter().all(|(lookup, ..)| lookup.trim().is_empty()));
    }

    /// The kept divergence of R22 ([`TypeName`]): a C++ `new ::Widget()`
    /// names `::Widget` for the index, which no type declares, and `Widget`
    /// for the overlay, whose last-segment retry binds it. A qualifier that
    /// is not empty names `Widget` on both.
    #[test]
    fn test_constructed_type_empty_qualifier_differs_by_type_name() {
        let fake = Fake::new(Layout::Index, &[("src/widget.hpp", "Widget", C, None)]);
        let declares = |name: &str| name == "Widget";
        let resolve = |path: &str, _bind: bool| (path == "Widget").then_some((0, 0.7));
        let construct = |path, split| {
            constructed_type(
                &fake,
                Language::Cpp,
                CallSite::Construct(path),
                &[],
                split,
                declares,
                resolve,
            )
        };
        assert_eq!(construct("::Widget", TypeName::Qualifier), None);
        assert_eq!(construct("::Widget", TypeName::LastSegment), Some((0, 0.7)));
        for split in [TypeName::Qualifier, TypeName::LastSegment] {
            assert_eq!(
                construct("shop::Widget", split),
                Some((0, 0.7)),
                "{split:?}"
            );
        }
    }

    /// R22: the whole path resolves first; a qualified miss retries its last
    /// segment where the site allows it, without the import tiers in a
    /// fully qualified language (PHP) and with them elsewhere (TS). A
    /// Java `new` never retries. An import alias passes the gate, and a
    /// resolved type that is no Class / Struct binds nothing.
    #[test]
    fn test_constructed_type_last_segment_retry_and_gate_return_each_verdict() {
        let fake = Fake::new(
            Layout::Index,
            &[
                ("src/Item.php", "Item", C, None),
                ("src/Shape.ts", "Shape", NodeKind::Interface, None),
            ],
        );
        let declares = |name: &str| name == "Item";
        let asked: RefCell<Vec<(String, bool)>> = RefCell::default();
        let run = |language, site, imports: &[RawImport]| {
            asked.borrow_mut().clear();
            constructed_type(
                &fake,
                language,
                site,
                imports,
                TypeName::Qualifier,
                declares,
                |path: &str, bind: bool| {
                    asked.borrow_mut().push((path.to_owned(), bind));
                    match path {
                        "Item" => Some((0, 0.7)),
                        "Shape" => Some((1, 0.7)),
                        _ => None,
                    }
                },
            )
        };
        let asked_for = |want: &[(&str, bool)]| {
            let want: Vec<(String, bool)> = want.iter().map(|&(p, b)| (p.to_owned(), b)).collect();
            assert_eq!(*asked.borrow(), want);
        };
        assert_eq!(
            run(Language::Php, CallSite::Construct("App\\Item"), &[]),
            Some((0, 0.7))
        );
        asked_for(&[("App\\Item", true), ("Item", false)]);
        assert_eq!(
            run(Language::TypeScript, CallSite::Construct("shop.Item"), &[]),
            Some((0, 0.7))
        );
        asked_for(&[("shop.Item", true), ("Item", true)]);
        assert_eq!(
            run(Language::Java, CallSite::Construct("x.Item"), &[]),
            None
        );
        asked_for(&[("x.Item", true)]);
        assert_eq!(run(Language::Go, CallSite::Plain("Item"), &[]), None);
        asked_for(&[]);
        let alias = [import("./item", "Item", Some("Renamed"))];
        assert_eq!(
            run(Language::Python, CallSite::Plain("Renamed"), &alias),
            None,
            "the alias passes the gate; the fake's lookup misses `Renamed`"
        );
        asked_for(&[("Renamed", true)]);
        let shape = [import("./shape", "Item", Some("Shape"))];
        assert_eq!(
            run(Language::Python, CallSite::Plain("Shape"), &shape),
            None,
            "an interface is not constructed"
        );
        asked_for(&[("Shape", true)]);
    }
}
