//! The order of the name tiers, once for the index
//! (`Resolver::resolve_symbol_with_import_binding`) and the session overlay
//! (`session/view/resolution.rs`): Tier 1 (the caller's file, then the
//! parts of a C# partial type), Tier 2 (the import-member tier, else the
//! named-import tier), Tier 2.5 (a qualified name: [`ladder::qualified`],
//! then the receiver-typing ladder and the Kotlin extension retry), Tier
//! 2.75 (the caller's bases, then the wildcard imports) and Tier 3 (one
//! name across the project).
//!
//! The rules of each tier live in [`ladder`], [`reexport`] and
//! [`type_ladder`](crate::analyzer::type_ladder). This module decides which
//! tier runs, in which order, and where the lookup stops. Each side answers
//! through its [`NameTierFacts`]: the index over its `SymbolTable`, with the
//! decision dump ([`NameTierFacts::record`]); the overlay over the archived
//! graph and the dirty parses, with its memos inside its methods.

use std::sync::Arc;

use serde::Serialize;

use crate::analyzer::import_binding::{
    fqn_language, import_member_fallback, plain_binding, MemberOwner,
};
use crate::analyzer::ladder::{
    self, split_qualifier, CallPick, ImportMember, ImportMembers, ImportModules, MemberHit,
    ModuleTreeFacts, Namespace, QualifiedSite, QualifierFacts, ResolveFacts, StemFile, TreeItem,
};
use crate::analyzer::outcome::{NoEdge, Resolution, Tier};
use crate::analyzer::reexport::{self, ImportRef, ModuleFacts, NamedImport, ResolveTarget};
use crate::analyzer::rust_paths::is_rust_source;
use crate::analyzer::same_file_pick::PickFacts;
use crate::analyzer::type_ladder::TypeMember;
use crate::analyzer::types::RawImport;
use crate::file_category::{FileMeta, Language};

/// The tier one name lookup ended at, for the index's decision dump.
/// Distinct from [`Tier`] because that enum models only resolution
/// *successes*, whereas the dump also needs to record the `Unresolved`
/// outcome to let the verification harness compute false negatives.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum DecisionTier {
    SameFile,
    ImportScoped,
    /// Tier 2.5 — qualifier-scoped lookup succeeded (the analyzer's
    /// `ResolutionTier::QualifierScoped`).
    QualifierScoped,
    /// Tier 2.75 — heritage-scoped lookup succeeded (the analyzer's
    /// `ResolutionTier::HeritageScoped`).
    HeritageScoped,
    Global,
    /// Tier 3 produced ≥2 kind-filtered candidates and suppressed the edge.
    /// Distinct from `Unresolved` (=0 candidates) so the verification
    /// harness can tell "no defence needed" from "defence fired" without
    /// needing to inspect `alt_count`. Edge behaviour is unchanged — both
    /// outcomes emit no edge.
    AmbiguousGlobal,
    /// Tier 3.5 — Rust workspace module-tree resolved the FQN path to a
    /// concrete file and found the member there. Confidence 1.0; tagged
    /// `reason: "module-tree"` so analytics can distinguish from Tier-4
    /// heuristic edges (confidence 0.7).
    ModuleTree,
    Unresolved,
    /// Receiver-typing ladder: the qualifier names one project type, and
    /// that type owns the member (in its file, or for Rust impls / Go
    /// methods / C# partials / Swift extensions, in its crate or package).
    TypeOwned,
    /// Receiver-typing ladder: a resolved declared supertype of the
    /// qualifier's one project type owns the member.
    TypeHeritage,
    /// Receiver-typing ladder: the qualifier names several project types;
    /// exactly one owns the member and every other provably does not.
    TypeCandidates,
    /// A call whose candidates are copies of one declaration in duplicated
    /// source trees took the copy nearest the caller by path (the
    /// analyzer's `ResolutionTier::NearestCopy`), at the ladder or at Tier 3.
    NearestCopy,
}

/// One tier decision of a lookup in [`NameTierFacts::source`].
#[derive(Debug, Clone, Copy)]
pub struct Decision<'d> {
    pub name: &'d str,
    pub specifier: Option<&'d str>,
    pub tier: DecisionTier,
    pub target: Option<u32>,
    /// The other same-named declarations the dump counts.
    pub alt_count: u32,
    pub confidence: Option<f32>,
}

/// What a lookup carries besides its name: the index's
/// `resolve_symbol_with_import_binding` arguments.
#[derive(Debug, Clone, Copy)]
pub struct TierPolicy<'c> {
    /// The kinds the lookup may bind.
    pub target: ResolveTarget,
    /// Whether the import tiers may bind the name. A type lookup in a fully
    /// qualified language still takes the named-import tier without it.
    pub bind_import: bool,
    /// The call site, `None` for a plain symbol lookup. Its presence admits
    /// the partial-type tier and the nearest copy of Tier 3.
    pub call: Option<CallPick<'c>>,
    /// The declared bases of the caller's enclosing type, which the
    /// heritage tier probes.
    pub heritage: &'c [String],
}

impl<'c> TierPolicy<'c> {
    /// A lookup outside a call site (`Resolver::resolve_symbol_with_heritage`):
    /// the import tiers bind, except a type in a fully qualified language,
    /// whose binding must not change the heritage graph underneath the
    /// receiver ladder.
    pub fn lookup(language: Language, target: ResolveTarget, heritage: &'c [String]) -> Self {
        Self {
            target,
            bind_import: target != ResolveTarget::Type || !fqn_language(language),
            call: None,
            heritage,
        }
    }
}

/// What [`name_tiers`] reads per side for one caller file
/// ([`Self::source`]). The shared rule facts come back through the
/// accessors; the methods below are the tiers whose facts or memos differ.
pub trait NameTierFacts {
    /// The declarations Tier 1, the heritage tier and Tier 3 pick from.
    type Decls: ResolveFacts;
    /// The import-member tier's module facts.
    type Imports: ImportMembers;
    /// The qualifier tier's facts.
    type Qualifiers: QualifierFacts;
    /// The module facts the hiding rule of Tier 3 reads.
    type Modules: ModuleFacts;
    /// Which file the qualifier tier's module-file step hands back
    /// ([`StemFile`]).
    const STEM: StemFile;

    fn decls(&self) -> &Self::Decls;
    fn import_members(&self) -> &Self::Imports;
    fn qualifiers(&self) -> &Self::Qualifiers;
    fn modules(&self) -> &Self::Modules;
    /// The caller's file.
    fn source(&self) -> &str;
    /// The caller file's meta, which the global barriers weigh.
    fn caller(&self) -> FileMeta;
    /// The language the side resolves the caller's names in.
    fn language(&self) -> Language;

    /// The member a bare C# call `name()` reaches in its caller's own type
    /// `owner` across the parts of a `partial` type: an owned
    /// ([`Tier::TypeOwned`]) or inherited ([`Tier::TypeHeritage`]) member,
    /// else `None`.
    fn partial_type_member(
        &self,
        name: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> Option<(u32, Tier)>;
    /// Tier 2 outside the import-member policy: [`reexport::named_import`]
    /// (only a TS / JS caller follows a re-export), else a Rust `use` the
    /// module tree places ([`rust_tree_import`]).
    fn named_import(
        &self,
        name: &str,
        imports: &[RawImport],
        target: ResolveTarget,
    ) -> Option<NamedImport>;
    /// The Rust module-tree lookup of the qualified `callee`
    /// ([`ladder::module_tree_member`]); `None` without a tree.
    fn module_tree_member(&self, callee: &str, member: &str, target: ResolveTarget) -> Option<u32>;
    /// The qualifier tier ([`ladder::qualified`]) for `site`, and the base
    /// a Python `super()` call bound through.
    fn qualified<'h>(
        &self,
        site: QualifiedSite<'_>,
        heritage: &'h [String],
    ) -> (Resolution, Option<&'h str>) {
        ladder::qualified(self.qualifiers(), site, heritage, Self::STEM, || {
            self.module_tree_member(site.callee, site.member, site.target)
        })
    }
    /// The receiver-typing ladder for the qualified `callee` = `qualifier`
    /// + `member`, which runs where the qualifier tier found nothing.
    fn receiver_ladder(
        &self,
        imports: &[RawImport],
        names: (&str, &str, &str),
        target: ResolveTarget,
        call: bool,
    ) -> TypeMember;
    /// Tier 3 for `name` ([`global_tier`] over the side's candidates).
    /// `nearest` (a call site) admits the nearest copy; `free_only` is a
    /// bare `f()` that never reaches a member.
    fn global(
        &self,
        name: &str,
        imports: &[RawImport],
        target: ResolveTarget,
        nearest: bool,
        free_only: bool,
    ) -> Resolution;
    /// The file the declaration `id` lives in.
    fn decl_file(&self, id: u32) -> Option<&str>;
    /// Whether the side keeps a decision dump: [`Self::record`] runs only
    /// then, so its inputs cost nothing on the production path.
    fn records(&self) -> bool {
        false
    }
    fn record(&self, _decision: Decision<'_>) {}
    /// The same-named declarations the dump counts for `name`.
    fn global_match_count(&self, _name: &str) -> u32 {
        0
    }
}

/// The module a barrel of the import tiers is reported as.
type Barrel<F> = <<F as NameTierFacts>::Imports as ImportModules>::Module;

/// The tiers in order for `name` with the caller's `imports`, under
/// `policy`. `barrel` receives the barrel an import-tier hit went through
/// (`Resolver::resolve_import`); `None` where no caller reads it. An empty
/// `name` and a name no tier binds both give `None`, never a guess.
///
/// 1. Tier 1 ([`ladder::same_file`]), then at a call site that reaches a
///    member the parts of the caller's C# partial type.
/// 2. Tier 2 where the import tiers may bind and `target` is no field: in an
///    import-member language [`ladder::import_member`] (a hit, an external
///    name that stops the lookup, a Kotlin extension retry, a Python retry
///    without imports, or the later tiers with the wildcards left open);
///    elsewhere [`NameTierFacts::named_import`].
/// 3. A qualified name resolves through its qualifier or not at all:
///    [`NameTierFacts::qualified`], then the receiver-typing ladder, then
///    for Kotlin the extension retry ([`ladder::kotlin_extension_retry`]).
/// 4. The caller's bases ([`ladder::heritage`]), then the wildcard imports
///    an explicit import left open ([`ladder::wildcard_member`]).
/// 5. Tier 3 ([`NameTierFacts::global`]).
pub fn name_tiers<F: NameTierFacts>(
    facts: &F,
    name: &str,
    imports: &[RawImport],
    policy: TierPolicy<'_>,
    barrel: Option<&mut Option<Arc<str>>>,
) -> Option<(u32, f32)>
where
    Barrel<F>: Into<Arc<str>>,
{
    let TierPolicy {
        target,
        bind_import,
        call,
        heritage,
    } = policy;
    let source = facts.source();
    let language = facts.language();
    let free_only = call.is_some_and(|call| call.free_only);

    // Tier 1 is kind-aware: a property named `Foo` does not win the lookup
    // of a constructor call `Foo()` in the same file. When the file holds
    // several unrelated owners of the name it binds nothing and the later
    // tiers decide.
    if let Resolution::Bound(hit) =
        ladder::same_file(facts.decls(), source, name, target, call, heritage)
    {
        note(
            facts,
            name,
            None,
            DecisionTier::SameFile,
            Some(hit.id),
            0,
            Some(hit.confidence),
        );
        return Some((hit.id, hit.confidence));
    }

    // A bare call in one part of a C# partial type reaches what the
    // caller's type owns in any part, as `this.m()` does (codex ruling P-d).
    if let Some((id, tier)) = call
        .filter(|call| !call.untyped && !call.free_only)
        .and_then(|call| facts.partial_type_member(name, call.owner?, target))
    {
        let decision = match tier {
            Tier::TypeHeritage => DecisionTier::TypeHeritage,
            _ => DecisionTier::TypeOwned,
        };
        let confidence = tier.confidence();
        note(facts, name, None, decision, Some(id), 0, Some(confidence));
        return Some((id, confidence));
    }

    let legacy_type_import = target == ResolveTarget::Type && fqn_language(language);
    let has_imports = (bind_import || legacy_type_import)
        && !imports.is_empty()
        && target != ResolveTarget::Field;
    // A wildcard binds only when no explicit import binds the name, and
    // only after the caller's heritage: members in scope beat imports.
    let mut wildcard_open = false;
    if bind_import && has_imports && import_member_fallback(language) {
        match ladder::import_member(
            facts.import_members(),
            source,
            language,
            name,
            imports,
            target,
            heritage,
        ) {
            ImportMember::Bound(hit) => {
                return import_scoped(facts, name, hit.import, hit.id, hit.barrel, barrel);
            }
            ImportMember::Extension(member) => {
                return extension_fallback(facts, member, imports, target, heritage);
            }
            ImportMember::External => {
                note(facts, name, None, DecisionTier::Unresolved, None, 0, None);
                return None;
            }
            // An import that may be local keeps the remaining tiers: Python
            // under the member's short name, a fully qualified language
            // under the callee as written.
            ImportMember::Retry(retry) => {
                let policy = TierPolicy {
                    bind_import: target != ResolveTarget::Type || !fqn_language(language),
                    ..policy
                };
                return name_tiers(facts, retry, &[], policy, None);
            }
            ImportMember::Continue { wildcard } => wildcard_open = wildcard,
        }
    } else if has_imports {
        if let Some(hit) = facts.named_import(name, imports, target) {
            let import = &imports[hit.position];
            return import_scoped(facts, name, import, hit.decl, hit.barrel, barrel);
        }
    }

    // Tiers 1 and 2 are keyed by short names, and Tier 3's kind + unique
    // filter near-always rejects the common member names (`new`, `from`):
    // a qualified name (`A::new`, `Cls.method`) never reaches them.
    if let Some((qualifier, member)) = split_qualifier(name) {
        let site = QualifiedSite {
            source,
            caller: facts.caller(),
            language,
            imports,
            callee: name,
            qualifier,
            member,
            target,
            call,
        };
        let (resolution, base) = facts.qualified(site, heritage);
        match resolution {
            Resolution::Bound(hit) => {
                let decision = match hit.tier {
                    Some(Tier::ModuleTree) => DecisionTier::ModuleTree,
                    Some(Tier::TypeOwned) => DecisionTier::TypeOwned,
                    Some(Tier::TypeHeritage) => DecisionTier::TypeHeritage,
                    _ => DecisionTier::QualifierScoped,
                };
                note(
                    facts,
                    name,
                    base,
                    decision,
                    Some(hit.id),
                    0,
                    Some(hit.confidence),
                );
                return Some((hit.id, hit.confidence));
            }
            // A `super()` call no base owns: no later tier runs.
            Resolution::NoEdge(_) => {
                note(facts, name, None, DecisionTier::Unresolved, None, 0, None);
                return None;
            }
            Resolution::Unknown | Resolution::Shadowed => {}
        }
        // The receiver-typing ladder runs only where every tier above found
        // nothing, so it adds edges and never moves one.
        let verdict = facts.receiver_ladder(
            imports,
            (
                name,
                ladder::self_qualifier(language, qualifier, call),
                member,
            ),
            target,
            call.is_some(),
        );
        let decision = match verdict {
            TypeMember::Owned(_) => DecisionTier::TypeOwned,
            TypeMember::Inherited(_) => DecisionTier::TypeHeritage,
            TypeMember::Candidate(_) => DecisionTier::TypeCandidates,
            TypeMember::NearestCopy(_) => DecisionTier::NearestCopy,
            TypeMember::Ambiguous => DecisionTier::AmbiguousGlobal,
            TypeMember::NoEdge => DecisionTier::Unresolved,
        };
        if let Some((id, tier)) = Tier::of_member(verdict) {
            let confidence = tier.confidence();
            note(facts, name, None, decision, Some(id), 0, Some(confidence));
            return Some((id, confidence));
        }
        // A Kotlin receiver named by a class import was the bare member
        // before the import tier typed it, which finds an extension function
        // on the type in another file.
        let extension = match verdict {
            TypeMember::NoEdge => ladder::kotlin_extension_retry(
                language,
                name,
                (qualifier, member),
                imports,
                heritage,
            ),
            _ => None,
        };
        if let Some(member) = extension {
            return extension_fallback(facts, member, imports, target, heritage);
        }
        if facts.records() {
            let alt_count = facts.global_match_count(member);
            note(facts, name, None, decision, None, alt_count, None);
        }
        return None;
    }

    // Each base of the caller, in declared order (the parser's source
    // order, mirroring MRO precedence), is an implicit qualifier; the first
    // whose file holds the member decides. This resolves `class Bar;
    // include Foo; end` and a Java / Kotlin / C# subclass calling an
    // inherited method without `this.`.
    if let (Resolution::Bound(hit), Some(base)) =
        ladder::heritage(facts.decls(), heritage, name, target, call, |base| {
            ladder::qualifier_file(
                facts.qualifiers(),
                source,
                facts.caller(),
                imports,
                (None, base, name),
                target,
                F::STEM,
            )
        })
    {
        note(
            facts,
            name,
            Some(base),
            DecisionTier::HeritageScoped,
            Some(hit.id),
            0,
            Some(hit.confidence),
        );
        return Some((hit.id, hit.confidence));
    }

    if let Some(hit) = ladder::wildcard_member(
        facts.import_members(),
        wildcard_open,
        source,
        language,
        name,
        imports,
        target,
    ) {
        return import_scoped(facts, name, hit.import, hit.id, hit.barrel, barrel);
    }

    facts
        .global(name, imports, target, call.is_some(), free_only)
        .edge()
}

/// The import tiers alone (`Resolver::resolve_import_tiers`): the
/// import-member tier, explicit imports then wildcards, where the language
/// binds through an import's member and `bind_import` holds, else the
/// named-import tier. No name tier, no heritage: what a name that a lexical
/// scope hides, an imported type and a Java field still reach.
pub fn import_tiers<F: NameTierFacts>(
    facts: &F,
    name: &str,
    imports: &[RawImport],
    bind_import: bool,
    target: ResolveTarget,
    barrel: Option<&mut Option<Arc<str>>>,
) -> Option<(u32, f32)>
where
    Barrel<F>: Into<Arc<str>>,
{
    // `ResolveTarget::Field` reaches here only for a bare name an import
    // may bind (Java `import static app.Consts.TABLE`); an `obj.field` read
    // never takes an import tier ([`ladder::field_read`]).
    if imports.is_empty() {
        return None;
    }
    let language = facts.language();
    if !(bind_import && import_member_fallback(language)) {
        let hit = facts.named_import(name, imports, target)?;
        let import = &imports[hit.position];
        return import_scoped(facts, name, import, hit.decl, hit.barrel, barrel);
    }
    match ladder::imported_member(
        facts.import_members(),
        facts.source(),
        language,
        name,
        imports,
        target,
        true,
    ) {
        MemberHit::Bound(hit) => import_scoped(facts, name, hit.import, hit.id, hit.barrel, barrel),
        MemberHit::Unbound { .. } => None,
    }
}

/// R17, the bare `member` of a Kotlin call through a class binding
/// ([`ladder::extension_fallback`]): the name tiers outside a call scope,
/// with the caller's `heritage`, top-level functions only.
pub fn extension_fallback<F: NameTierFacts>(
    facts: &F,
    member: &str,
    imports: &[RawImport],
    target: ResolveTarget,
    heritage: &[String],
) -> Option<(u32, f32)>
where
    Barrel<F>: Into<Arc<str>>,
{
    let policy = TierPolicy::lookup(facts.language(), target, heritage);
    ladder::extension_fallback(facts.decls(), member, |member| {
        name_tiers(facts, member, imports, policy, None)
    })
    .next()
}

/// Tier 3 over `candidates`, the same-named declarations of the field the
/// lookup weighs with their files' metas ([`ladder::global`]): emit only
/// when the kind-filtered set is unique, or at a call site (`nearest`) the
/// copy nearest the caller. Refusing to guess on ambiguity is the dominant
/// defence against bare-name fan-out (`new`, `format`, `default`, `main`).
/// An import that puts the pick out of reach hides it
/// ([`reexport::import_hides`]). The dump records the raw same-name count
/// so the verification harness can tell suppressed-ambiguous from
/// truly-unresolved.
pub fn global_tier<F: NameTierFacts, I: Iterator<Item = (u32, FileMeta)>>(
    facts: &F,
    name: &str,
    imports: &[RawImport],
    target: ResolveTarget,
    nearest: bool,
    candidates: impl Fn() -> I,
) -> Resolution {
    let source = facts.source();
    let hidden = |id: u32| {
        facts.decl_file(id).is_some_and(|module| {
            reexport::import_hides(
                facts.modules(),
                source,
                name,
                imports.iter().map(ImportRef::from),
                module,
                || PickFacts::owner(facts.decls(), id).is_some(),
            )
        })
    };
    let resolution = ladder::global(
        facts.decls(),
        source,
        facts.caller(),
        target,
        nearest,
        candidates,
        hidden,
    );
    if facts.records() {
        let specifier = imports
            .iter()
            .find(|i| match &i.alias {
                Some(alias) => alias == name,
                None => i.imported_name == name,
            })
            .map(|i| i.source.as_str());
        let raw_count = facts.global_match_count(name);
        let (tier, target_id, confidence, alt_count) = match resolution {
            Resolution::Bound(hit) => {
                let tier = match hit.tier {
                    Some(Tier::NearestCopy) => DecisionTier::NearestCopy,
                    _ => DecisionTier::Global,
                };
                (
                    tier,
                    Some(hit.id),
                    Some(hit.confidence),
                    raw_count.saturating_sub(1),
                )
            }
            Resolution::NoEdge(NoEdge::Ambiguous(_)) => {
                (DecisionTier::AmbiguousGlobal, None, None, raw_count)
            }
            _ => (DecisionTier::Unresolved, None, None, raw_count),
        };
        facts.record(Decision {
            name,
            specifier,
            tier,
            target: target_id,
            alt_count,
            confidence,
        });
    }
    resolution
}

/// The first Rust `use` binding `name` whose path the module tree places
/// ([`ladder::module_tree_item`]), with the module-level item it names
/// there: `use crate::f;` where `lib.rs` writes `pub use inner::f;` binds
/// `inner.rs`'s `f`, which the candidate probe of `lib.rs` misses, and
/// `use crate::util::helper;` binds the caller crate's `util.rs` though
/// another crate holds a `util.rs` too. A `use` names no associated item,
/// so an owned namesake never binds. An importer outside every Cargo target
/// binds nothing here: the tree cannot tell which crate its own manifest
/// names (a duplicated source tree outside the workspace), so the
/// nearest-copy tier decides. Returns the import's position and the item.
pub fn rust_tree_import<T: ModuleTreeFacts, M: ImportMembers + ?Sized>(
    tree: &T,
    members: &M,
    importer: &str,
    name: &str,
    imports: &[RawImport],
    target: ResolveTarget,
) -> Option<(usize, u32)> {
    if !is_rust_source(std::path::Path::new(importer)) {
        return None;
    }
    imports.iter().enumerate().find_map(|(position, import)| {
        let imported = plain_binding(&import.imported_name, import.alias.as_deref(), name)?;
        tree.caller_crate(importer)?;
        let TreeItem::Found { file, item } = ladder::module_tree_item(
            tree,
            importer,
            &format!("{}::{imported}", import.source),
            Namespace::of(target),
        )?
        else {
            return None;
        };
        members
            .member_in_file(file.as_ref(), &item, target, MemberOwner::TopLevel)
            .map(|decl| (position, decl))
    })
}

/// Record a Tier 2 hit through `import`, report its `hit_barrel`, and give
/// its edge.
fn import_scoped<F: NameTierFacts, M: Into<Arc<str>>>(
    facts: &F,
    name: &str,
    import: &RawImport,
    id: u32,
    hit_barrel: Option<M>,
    barrel: Option<&mut Option<Arc<str>>>,
) -> Option<(u32, f32)> {
    if let Some(barrel) = barrel {
        *barrel = hit_barrel.map(Into::into);
    }
    let confidence = Tier::ImportScoped.confidence();
    note(
        facts,
        name,
        Some(import.source.as_str()),
        DecisionTier::ImportScoped,
        Some(id),
        0,
        Some(confidence),
    );
    Some((id, confidence))
}

fn note<F: NameTierFacts>(
    facts: &F,
    name: &str,
    specifier: Option<&str>,
    tier: DecisionTier,
    target: Option<u32>,
    alt_count: u32,
    confidence: Option<f32>,
) {
    if facts.records() {
        facts.record(Decision {
            name,
            specifier,
            tier,
            target,
            alt_count,
            confidence,
        });
    }
}
