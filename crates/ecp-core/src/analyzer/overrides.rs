//! `Overrides` edges: a method → the nearest applicable declaration of its
//! name along the declared bases of its type.
//!
//! The index's post-process pass (`ecp-analyzer` `post_process::overrides`)
//! and the overlay's replay for a dirty file (`session::view`) both call
//! [`method_lookup`] once per callable and [`MethodLookup::edge`] once per
//! stop it returns, so a dirty file gets the edges a reindex gives where
//! their facts agree. Each side answers [`OverrideFacts`] from its own
//! tables: the index from its `SymbolTable` (the declared bases Pass 1.9
//! resolved) and every parse, the overlay from each dirty file's fresh parse
//! and the base graph. The index alone records the BlindSpots below; the
//! overlay records none.
//!
//! # The declared base, not a same-named type
//!
//! The owner type of a method is the innermost enclosing type that holds
//! heritage ([`holds_heritage`], the kinds `enclosing_class_heritage` reads),
//! when no callable sits between them: a local function is no member. A
//! method declared outside its type (a Go receiver method, a Rust `impl`
//! block member, a C++ out-of-line definition) takes the one type of its
//! file that its `owner_class` names ([`owner_types`]). The bases of that
//! type are the ones Pass 1.9 resolved through the type's imports and scope
//! ([`OverrideFacts::declared_bases`]), so another type that only shares a
//! base's name is never reached.
//!
//! The parts of a C# `partial` type in one project are one type
//! (`c_sharp::partial`): every part reads the bases any part lists, a base
//! binds the type once, and the lookup weighs the methods of all parts, each
//! edge ending at the method in the part that declares it. Parts whose
//! project is unknown stay types of their own, and a base naming them stays
//! [`BS_KIND_OVERRIDE_BASE_AMBIGUOUS`].
//!
//! # Evidence per language
//!
//! | Rule | Languages | Candidate | Edge |
//! |---|---|---|---|
//! | `Marked` | Java, Kotlin, C#, C++, others | a method the parser marked (`@Override`, `override`) | the path confidence, `Exact` |
//! | `Inferred` | TypeScript, JavaScript, Python, PHP, Ruby, Swift, Dart | a marked method, else any method of a type with bases | marked: the path confidence, `Exact`; unmarked: [`NAME_MATCH_CONFIDENCE`], `Heuristic` |
//! | `TraitImpl` | Rust | a method of an `impl Trait for T` block, which carries the trait as heritage | the path confidence, `Exact`, only into that trait, never a supertrait |
//! | `NoConcept` | Go | none | no edge; a [`BS_KIND_NO_OVERRIDE_CONCEPT`] BlindSpot where a method shadows one of an embedded type |
//!
//! C has no override ([`rules_for`] returns `None`). The parsers of the
//! `Inferred` languages capture no mandatory marker, so an unmarked method
//! there pairs with its base by name alone, and the edge stays below
//! `HEURISTIC_CEILING` with its own `reason` (C4 evidence classes).
//!
//! The path confidence is the weakest resolver confidence on the path the
//! lookup walked (`evidence`): 1.0 for a base in the same file, 0.95
//! through an import, 0.85 through a qualifier, 0.8 through the heritage
//! scope. A path through a base bound by a bare name alone (the `Global`
//! tier, 0.7, at `HEURISTIC_CEILING`) gives [`NAME_MATCH_CONFIDENCE`] with
//! reason [`OVERRIDES_BASE_BY_NAME_REASON`] instead, marked or not. A marked
//! method whose signature the lookup did not confirm gives
//! [`NAME_MATCH_CONFIDENCE`] too.
//!
//! # Nearest declaration
//!
//! The lookup climbs the resolved bases and stops at the nearest type that
//! declares an applicable method of the name (below): for `C → B → A`, `C.m → A.m` when B lacks `m`, and
//! `C.m → B.m` when B declares it. Each language keeps its own order:
//!
//! | Order | Languages | Lookup |
//! |---|---|---|
//! | per base | Java, Kotlin, C#, C++, TypeScript, JavaScript, PHP, Swift | each declared base is a branch, and each branch ends at its nearest declaration: the class chain and every interface give one edge each |
//! | C3 MRO | Python | the first class of the method resolution order |
//! | ancestors | Ruby | the mixins, last included first, then the superclass, depth first |
//! | mixin chain | Dart | the mixins, last first, then the superclass; each interface a branch of its own |
//!
//! The same declaration reached twice (a diamond) is one edge
//! ([`OverrideEdges`]). Distance does not lower the confidence; the weakest
//! resolver confidence on the path does.
//!
//! # Applicable declaration
//!
//! A declaration of the name applies when its signature, read from the
//! function meta paired by span, makes it the overridden one (`Matcher::fit`).
//! An overload or a static is skipped and the lookup goes on past it; an
//! undecided one ends the path with no edge:
//!
//! | Signature | Languages | Applies | Skipped | No edge |
//! |---|---|---|---|---|
//! | typed | Java, Kotlin, C#, C++, Swift | equal arity, equal parameter types (whitespace stripped, generics as text) | another arity, another type | a side without meta, a blank parameter type, a type difference through a base with type arguments; C++: a base method not declared virtual; C#: parameter types that differ only in a nullable annotation (`string?` is `string`, `int?` is not `int`) |
//! | arity | TypeScript, PHP, Dart | equal arity, or a visible rest parameter (name only) | — | another arity |
//! | lenient | Python, Ruby, JavaScript | equal arity; another arity or no meta by name only | — | — |
//!
//! Statics: Java, Kotlin, C#, C++ and Dart skip a static method on either
//! side; Swift, TypeScript, JavaScript, PHP, Python and Ruby pair a static
//! only with a static. A marker never makes a pair confirmed by name only
//! `Exact`. Rust and Go read no signature.
//!
//! # BlindSpots
//!
//! The lookup never skips a gap to reach a farther ancestor. A gap ends its
//! path with a [`Stop`] the index records as a BlindSpot at the method,
//! deduplicated per method and base:
//!
//! | Kind | Gap |
//! |---|---|
//! | [`BS_KIND_OVERRIDE_BASE_UNRESOLVED`] | a declared base resolved to no project type (external, or a resolution failure) |
//! | [`BS_KIND_OVERRIDE_BASE_AMBIGUOUS`] | a declared base names several project types, or one picked by path proximity alone |
//! | [`BS_KIND_OVERRIDE_AMBIGUOUS`] | the nearest declaring base has several applicable declarations, or one the signature leaves undecided (table above) |
//! | [`BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE`] | a heritage cycle, more than [`MAX_OVERRIDE_DEPTH`] levels, or no consistent Python MRO |

use crate::analyzer::heritage::{holds_heritage, HeritageNode};
use crate::analyzer::types::{
    span_contains, RawFunctionMeta, RawNode, NEAREST_COPY_CONFIDENCE, OVERRIDE_SENTINEL,
};
use crate::contracts::raw_records::HEURISTIC_CEILING;
use crate::file_category::Language;
use crate::graph::{FunctionMeta, NodeKind};
use rustc_hash::FxHashMap;
use std::cmp::Reverse;
use std::collections::hash_map::Entry;

/// BlindSpot kind: a Go method shadows a same-name method of a type its
/// receiver embeds. Go has no override, so no `Overrides` edge exists.
pub const BS_KIND_NO_OVERRIDE_CONCEPT: &str = "no-override-concept";

/// BlindSpot kind: the nearest declaring base on the lookup path has several
/// applicable declarations of the name, or one whose signature leaves the
/// override undecided.
pub const BS_KIND_OVERRIDE_AMBIGUOUS: &str = "override-ambiguous";

/// BlindSpot kind: a declared base on the lookup path resolved to no project
/// type, so the method may override a member the graph does not hold.
pub const BS_KIND_OVERRIDE_BASE_UNRESOLVED: &str = "override-base-unresolved";

/// BlindSpot kind: a declared base on the lookup path names several
/// distinct project types, or one the resolver picked by path proximity
/// alone.
pub const BS_KIND_OVERRIDE_BASE_AMBIGUOUS: &str = "override-base-ambiguous";

/// BlindSpot kind: the lookup met a heritage cycle, the depth cap, or bases
/// with no consistent Python method resolution order.
pub const BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE: &str = "override-hierarchy-incomplete";

/// Every BlindSpot kind the index's pass records.
pub const BS_KINDS: [&str; 5] = [
    BS_KIND_NO_OVERRIDE_CONCEPT,
    BS_KIND_OVERRIDE_AMBIGUOUS,
    BS_KIND_OVERRIDE_BASE_UNRESOLVED,
    BS_KIND_OVERRIDE_BASE_AMBIGUOUS,
    BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE,
];

/// The confidence of an edge paired by name alone: `Heuristic`, below the
/// 0.7 ceiling.
pub const NAME_MATCH_CONFIDENCE: f32 = 0.6;

/// The heritage levels the lookup climbs above the method's type before it
/// records [`BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE`].
pub const MAX_OVERRIDE_DEPTH: usize = 32;

/// The reason of an `Exact` edge: a marked (or Rust trait-impl) method whose
/// signature the lookup confirmed, through a base bound above the ceiling.
pub const OVERRIDES_REASON: &str = "post_process:overrides";

/// The reason of an edge paired by name alone ([`NAME_MATCH_CONFIDENCE`]).
pub const OVERRIDES_NAME_MATCH_REASON: &str = "post_process:overrides:name-in-base";

/// The reason of an edge whose path crosses a base bound by a bare name
/// alone ([`NAME_MATCH_CONFIDENCE`]).
pub const OVERRIDES_BASE_BY_NAME_REASON: &str = "post_process:overrides:base-by-name";

/// Every reason an `Overrides` edge carries.
pub const OVERRIDES_REASONS: [&str; 3] = [
    OVERRIDES_REASON,
    OVERRIDES_NAME_MATCH_REASON,
    OVERRIDES_BASE_BY_NAME_REASON,
];

/// One declaration of a method name in a type: its node, and the function
/// meta the parser paired with it by span (as Pass 1.8 does).
#[derive(Debug, Clone, Copy)]
pub struct Decl<'a> {
    pub slot: u32,
    pub meta: Option<&'a RawFunctionMeta>,
}

/// `(type, method name)` → the methods so named that the type declares.
/// The type is an [`OverrideFacts::partial_type`] representative, and each
/// declaration keeps its own node, so an edge targets the method in the
/// part that declares it.
pub type MethodIndex<'a> = FxHashMap<(u32, &'a str), Vec<Decl<'a>>>;

/// One declared base of a type, as the derivation reads it.
pub trait OverrideBase {
    /// The heritage spelling the parser recorded. A blank one is no base;
    /// a `<` in it marks a base spelled with type arguments.
    fn spelling(&self) -> &str;
    /// The project types the spelling resolved to, each with the resolver's
    /// confidence, in resolver order. Never the declaring type. Empty when
    /// the spelling resolved to no project type.
    fn candidates(&self) -> &[(u32, f32)];
}

/// An owned [`OverrideBase`], for a caller that builds its rows itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BaseRow {
    pub spelling: String,
    pub candidates: Vec<(u32, f32)>,
}

impl OverrideBase for BaseRow {
    fn spelling(&self) -> &str {
        &self.spelling
    }

    fn candidates(&self) -> &[(u32, f32)] {
        &self.candidates
    }
}

/// The tables the lookup reads. Ids are the caller's node ids.
pub trait OverrideFacts {
    type Base: OverrideBase;
    /// The declared bases of the type `ty`, in declared order; empty when it
    /// declares none. Every part of a C# `partial` type reads the whole list.
    fn declared_bases(&self, ty: u32) -> &[Self::Base];
    /// The representative part of the C# `partial` type `ty`; `ty` itself
    /// for every other type (`SymbolTable::partial_type`).
    fn partial_type(&self, ty: u32) -> u32;
    /// The kind of the node `id`.
    fn node_kind(&self, id: u32) -> NodeKind;
    /// The declarations of the method `name` in the type `ty`, a
    /// [`Self::partial_type`] representative; `None` when it declares none.
    fn methods<'s>(&'s self, ty: u32, name: &'s str) -> Option<&'s [Decl<'s>]>;
}

/// The declaration fields [`owner_types`] reads.
pub trait OverrideNode: HeritageNode {
    fn name(&self) -> &str;
    fn owner_class(&self) -> Option<&str>;
}

impl OverrideNode for RawNode {
    fn name(&self) -> &str {
        &self.name
    }

    fn owner_class(&self) -> Option<&str> {
        self.owner_class.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverrideRule {
    Marked,
    Inferred,
    TraitImpl,
    NoConcept,
}

/// The order a language searches the bases of one type (`lookup_order`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chain {
    PerBase,
    Ancestors,
    Mixins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Walk {
    Chain(Chain),
    Mro,
}

/// How a language tells the declaration a method overrides from an
/// overload of it (`Matcher::fit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signature {
    /// An overloading language: equal arity and equal parameter types.
    /// `virtual_only` (C++) also requires a virtual base method.
    /// `nullable_annotations` (C#): `string?` and `string` are one type, but
    /// `int?` and `int` are two, and the text cannot tell them apart.
    Typed {
        virtual_only: bool,
        nullable_annotations: bool,
    },
    /// No overloading, and the compiler checks the override: an arity
    /// difference without a visible rest parameter is reported, not paired.
    Arity,
    /// No overloading, and the meta keeps no default value: an arity
    /// difference pairs by name alone.
    Lenient,
    /// The pass reads no signature (Rust, Go, the rest).
    Unchecked,
}

/// Whether a static method takes part in overriding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Statics {
    /// A static method neither overrides nor is overridden.
    Never,
    /// A static method overrides only a static one (a class-side method).
    Paired,
    Ignored,
}

/// The rules of one language: who takes part, the lookup order, the
/// signature check and the statics.
#[derive(Debug, Clone, Copy)]
pub struct Rules {
    rule: OverrideRule,
    walk: Walk,
    signature: Signature,
    statics: Statics,
}

/// The rules of a file of `language`; `None` for C, which has no override.
/// The index reads the language the file stored
/// (`SymbolTable::file_language`; the overlay's `OverlayFileInput::file_meta`):
/// a Vue `<script lang="ts">` file is TypeScript though its path names none.
/// A caller that must agree with it reads the stored language too.
pub fn rules_for(language: Language) -> Option<Rules> {
    use OverrideRule::*;
    use Signature::*;
    let per_base = Walk::Chain(Chain::PerBase);
    let typed = Typed {
        virtual_only: false,
        nullable_annotations: false,
    };
    let (rule, walk, signature, statics) = match language {
        Language::Java | Language::Kotlin => (Marked, per_base, typed, Statics::Never),
        Language::CSharp => (
            Marked,
            per_base,
            Typed {
                virtual_only: false,
                nullable_annotations: true,
            },
            Statics::Never,
        ),
        Language::Cpp => (
            Marked,
            per_base,
            Typed {
                virtual_only: true,
                nullable_annotations: false,
            },
            Statics::Never,
        ),
        // `class func` carries the static flag, and it overrides.
        Language::Swift => (Inferred, per_base, typed, Statics::Paired),
        Language::TypeScript | Language::Php => (Inferred, per_base, Arity, Statics::Paired),
        Language::JavaScript => (Inferred, per_base, Lenient, Statics::Paired),
        Language::Python => (Inferred, Walk::Mro, Lenient, Statics::Paired),
        Language::Ruby => (
            Inferred,
            Walk::Chain(Chain::Ancestors),
            Lenient,
            Statics::Paired,
        ),
        Language::Dart => (Inferred, Walk::Chain(Chain::Mixins), Arity, Statics::Never),
        Language::Rust => (TraitImpl, per_base, Unchecked, Statics::Ignored),
        Language::Go => (NoConcept, per_base, Unchecked, Statics::Ignored),
        Language::C => return None,
        _ => (Marked, per_base, Unchecked, Statics::Ignored),
    };
    Some(Rules {
        rule,
        walk,
        signature,
        statics,
    })
}

impl Rules {
    /// Whether a callable of `kind` takes part (`Some`), and whether an
    /// edge of it the signature confirms is `Exact`.
    fn exact_rule(self, kind: NodeKind, marked: bool, has_heritage: bool) -> Option<bool> {
        use OverrideRule::*;
        match self.rule {
            Marked if !marked => None,
            Inferred if !marked && kind == NodeKind::Constructor => None,
            TraitImpl if !has_heritage => None,
            Marked | TraitImpl => Some(true),
            Inferred => Some(marked),
            NoConcept => Some(false),
        }
    }
}

/// Why the nearest declaring type gives no edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ambiguity {
    /// This many declarations apply.
    Several(usize),
    /// A side has no meta, or a parameter type the parser left blank, or a
    /// type argument of a generic base may stand for a parameter type.
    NotComparable,
    /// A C++ base method not declared virtual: it may hide, not override.
    NotVirtual,
    /// C# parameter types that differ only in a nullable annotation.
    Nullable,
    /// A parameter count differs where the language has no overloading.
    Arity { base: usize, own: usize },
}

/// Whether one declaration is the one the method overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    /// Not a target: a static or an overload. The lookup goes on past it.
    Skip,
    /// The target. `confirmed` is false when the name alone pairs them.
    Match { confirmed: bool },
    /// Undecided: the lookup stops with no edge.
    Unknown(Ambiguity),
}

fn is_static(meta: Option<&RawFunctionMeta>) -> bool {
    meta.is_some_and(|m| m.flags & FunctionMeta::FLAG_STATIC != 0)
}

fn arity(meta: &RawFunctionMeta) -> usize {
    meta.params.len() / 2
}

/// A rest parameter the meta shows: `...rest` (JavaScript, TypeScript),
/// `*args` (Ruby).
fn has_rest(meta: &RawFunctionMeta) -> bool {
    meta.params
        .iter()
        .step_by(2)
        .any(|name| name.starts_with("...") || name.starts_with('*'))
}

/// Type texts equal once whitespace is gone; generics compare as text.
fn same_type(a: &str, b: &str) -> bool {
    a.chars()
        .filter(|c| !c.is_whitespace())
        .eq(b.chars().filter(|c| !c.is_whitespace()))
}

/// Type texts equal once whitespace and every `?` are gone: `string?`,
/// `List<int?>`.
fn same_but_nullable(a: &str, b: &str) -> bool {
    let bare = |s: &str| {
        s.chars()
            .filter(|c| !c.is_whitespace() && *c != '?')
            .collect::<Vec<_>>()
    };
    bare(a) == bare(b)
}

/// The method being looked up, against the declarations of its name.
#[derive(Debug, Clone, Copy)]
struct Matcher<'a> {
    own: Option<&'a RawFunctionMeta>,
    signature: Signature,
    statics: Statics,
}

impl Matcher<'_> {
    /// A static method where statics take no part never overrides.
    fn skips_method(&self) -> bool {
        self.statics == Statics::Never && is_static(self.own)
    }

    /// `generic`: a base on the path was spelled with type arguments.
    fn fit(&self, base: Option<&RawFunctionMeta>, generic: bool) -> Fit {
        match self.statics {
            Statics::Never if is_static(base) => return Fit::Skip,
            Statics::Paired if is_static(self.own) != is_static(base) => return Fit::Skip,
            _ => {}
        }
        let (own, base) = match (self.signature, self.own, base) {
            (Signature::Unchecked, ..) => return Fit::Match { confirmed: true },
            (_, Some(own), Some(base)) => (own, base),
            (Signature::Typed { .. }, ..) => return Fit::Unknown(Ambiguity::NotComparable),
            _ => return Fit::Match { confirmed: false },
        };
        let (own_arity, base_arity) = (arity(own), arity(base));
        match self.signature {
            Signature::Typed {
                virtual_only,
                nullable_annotations,
            } => {
                if own_arity != base_arity {
                    return Fit::Skip;
                }
                let mut blank = false;
                let mut nullable = false;
                for (a, b) in own.params.iter().zip(&base.params).skip(1).step_by(2) {
                    if a.trim().is_empty() || b.trim().is_empty() {
                        blank = true;
                    } else if nullable_annotations && same_but_nullable(a, b) {
                        nullable |= !same_type(a, b);
                    } else if !same_type(a, b) {
                        return if generic {
                            Fit::Unknown(Ambiguity::NotComparable)
                        } else {
                            Fit::Skip
                        };
                    }
                }
                if blank {
                    Fit::Unknown(Ambiguity::NotComparable)
                } else if nullable {
                    Fit::Unknown(Ambiguity::Nullable)
                } else if virtual_only && base.flags & FunctionMeta::FLAG_VIRTUAL == 0 {
                    Fit::Unknown(Ambiguity::NotVirtual)
                } else {
                    Fit::Match { confirmed: true }
                }
            }
            _ if own_arity == base_arity => Fit::Match { confirmed: true },
            Signature::Arity if !has_rest(own) && !has_rest(base) => {
                Fit::Unknown(Ambiguity::Arity {
                    base: base_arity,
                    own: own_arity,
                })
            }
            _ => Fit::Match { confirmed: false },
        }
    }

    /// Where the declarations of the name in `ty` end the lookup; `None`
    /// when none applies and the lookup goes on past `ty`.
    fn stop(&self, ty: u32, decls: &[Decl<'_>], identity: f32, generic: bool) -> Option<Stop> {
        let mut fits = decls
            .iter()
            .filter_map(|d| match self.fit(d.meta, generic) {
                Fit::Skip => None,
                Fit::Match { confirmed } => Some(Ok((d.slot, confirmed))),
                Fit::Unknown(why) => Some(Err(why)),
            });
        let first = fits.next()?;
        let more = fits.count();
        Some(match first {
            _ if more > 0 => Stop::Ambiguous {
                ty,
                why: Ambiguity::Several(more + 1),
            },
            Ok((target, confirmed)) => Stop::Found {
                ty,
                target,
                identity,
                confirmed,
            },
            Err(why) => Stop::Ambiguous { ty, why },
        })
    }
}

/// Java `@Override` (also fully qualified), Dart `@override`, Python
/// `@override` / `@typing.override`, and the Kotlin / C# / C++ / Swift
/// sentinel.
pub fn is_override_marker(decorator: &str) -> bool {
    decorator == OVERRIDE_SENTINEL
        || matches!(
            decorator.trim().trim_start_matches('@'),
            "Override"
                | "java.lang.Override"
                | "override"
                | "typing.override"
                | "typing_extensions.override"
        )
}

/// Whether a declaration's raw decorators mark it as an override.
pub fn is_marked(decorators: &[String]) -> bool {
    decorators.iter().any(|d| is_override_marker(d))
}

/// Why a lookup stopped short of the whole hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Cycle,
    Depth,
    Inconsistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Incomplete {
    /// The type where the lookup stopped.
    pub at: u32,
    pub why: Why,
}

/// Where the lookup of one name ended on one path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stop {
    /// The one declaration of the name in `ty` that applies. `identity` is
    /// the weakest resolver confidence on the path to `ty`; `confirmed` is
    /// false when the name alone pairs them.
    Found {
        ty: u32,
        target: u32,
        identity: f32,
        confirmed: bool,
    },
    /// `ty` declares the name, and no one declaration of it applies.
    Ambiguous {
        ty: u32,
        why: Ambiguity,
    },
    /// Declared base `index` of `owner` (`declared_bases(owner)[index]`)
    /// binds no one project type.
    Gap {
        owner: u32,
        index: usize,
    },
    Incomplete(Incomplete),
}

/// The one project type a base's `candidates` bind and the resolver's
/// confidence. `None` for no candidate, several, or one picked by path
/// proximity alone.
fn bind(candidates: &[(u32, f32)]) -> Option<(u32, f32)> {
    match *candidates {
        [(id, confidence)] if confidence > NEAREST_COPY_CONFIDENCE => Some((id, confidence)),
        _ => None,
    }
}

/// The confidence and reason of an edge whose path has `identity`.
///
/// A base bound by a unique bare name (the `Global` tier, at the ceiling)
/// never yields an `Exact` edge, even when marked. A base bound through an
/// import or a qualifier carries the resolver's confidence: a marker does
/// not raise it to 1.0.
fn evidence(exact_rule: bool, identity: f32) -> (f32, &'static str) {
    if identity <= HEURISTIC_CEILING {
        (NAME_MATCH_CONFIDENCE, OVERRIDES_BASE_BY_NAME_REASON)
    } else if exact_rule {
        (identity.min(1.0), OVERRIDES_REASON)
    } else {
        (NAME_MATCH_CONFIDENCE, OVERRIDES_NAME_MATCH_REASON)
    }
}

/// One callable as the lookup reads it.
#[derive(Debug, Clone, Copy)]
pub struct OverrideMethod<'a> {
    /// The declared name; the lookup trims it.
    pub name: &'a str,
    pub kind: NodeKind,
    /// The raw decorators: an override marker among them marks the method.
    pub decorators: &'a [String],
    /// The method's own heritage: a Rust trait-impl method carries its trait.
    pub heritage: &'a [String],
    /// The function meta paired with it by span.
    pub meta: Option<&'a RawFunctionMeta>,
}

/// One Python method resolution order: a type, or a declared base that
/// binds no one project type, kept as an opaque class with no bases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MroEntry {
    Type(u32),
    Gap { owner: u32, index: usize },
}

type Mro = Result<Vec<MroEntry>, Incomplete>;

/// The C3 orders computed so far, shared by every lookup of one run.
#[derive(Debug, Default)]
pub struct MroCache(FxHashMap<u32, Mro>);

/// The stops of one method's lookup.
#[derive(Debug, Clone)]
pub struct MethodLookup {
    exact_rule: bool,
    no_concept: bool,
    pub stops: Vec<Stop>,
}

/// One `Overrides` edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverrideEdge {
    pub source: u32,
    pub target: u32,
    pub confidence: f32,
    /// One of [`OVERRIDES_REASONS`].
    pub reason: &'static str,
}

impl MethodLookup {
    /// The edge `stop` gives the method at `source`: `None` for every stop
    /// but a [`Stop::Found`] at another node, and for that one too where
    /// the language has no override (Go). A marker never makes a branch the
    /// signature did not confirm `Exact`.
    pub fn edge(&self, source: u32, stop: Stop) -> Option<OverrideEdge> {
        match stop {
            Stop::Found {
                target,
                identity,
                confirmed,
                ..
            } if target != source && !self.no_concept => {
                let (confidence, reason) = evidence(self.exact_rule && confirmed, identity);
                Some(OverrideEdge {
                    source,
                    target,
                    confidence,
                    reason,
                })
            }
            _ => None,
        }
    }
}

/// The lookup of one callable whose owner type is `owner` (its
/// [`owner_types`] entry, as the caller numbers it). `None` when the method
/// takes no part: no name, not a callable, unmarked where the language
/// needs a marker, an unmarked constructor, a Rust method outside a trait
/// impl, a static where statics take no part, or an owner with no declared
/// base. Cost: per [`Stop`], one walk of the owner's ancestors, each type
/// once per path identity, at most [`MAX_OVERRIDE_DEPTH`] levels deep; the
/// Python order is computed once per type per [`MroCache`].
pub fn method_lookup<F: OverrideFacts>(
    facts: &F,
    rules: Rules,
    method: &OverrideMethod<'_>,
    owner: u32,
    mros: &mut MroCache,
) -> Option<MethodLookup> {
    let name = method.name.trim();
    if !method.kind.is_callable() || name.is_empty() {
        return None;
    }
    let exact_rule = rules.exact_rule(
        method.kind,
        is_marked(method.decorators),
        !method.heritage.is_empty(),
    )?;
    let matcher = Matcher {
        own: method.meta,
        signature: rules.signature,
        statics: rules.statics,
    };
    if matcher.skips_method() || facts.declared_bases(owner).is_empty() {
        return None;
    }
    let stops = match (rules.rule, rules.walk) {
        (OverrideRule::NoConcept, _) => embedded_namesakes(facts, matcher, owner, name),
        (OverrideRule::TraitImpl, _) => named_trait(facts, matcher, owner, name, method.heritage),
        (_, Walk::Mro) => mro_lookup(facts, matcher, owner, name, mros),
        (_, Walk::Chain(chain)) => {
            let mut search = Search {
                facts,
                matcher,
                chain,
                name,
                stops: Vec::new(),
                seen: FxHashMap::default(),
                path: vec![facts.partial_type(owner)],
            };
            search.bases(owner, 1.0, false);
            search.stops
        }
    };
    Some(MethodLookup {
        exact_rule,
        no_concept: rules.rule == OverrideRule::NoConcept,
        stops,
    })
}

/// The `Overrides` edges of one run, one per `(source, target)`: a pair
/// reached on two paths keeps the higher confidence and its reason, at the
/// position of its first edge.
#[derive(Debug, Default)]
pub struct OverrideEdges {
    edges: Vec<OverrideEdge>,
    at: FxHashMap<(u32, u32), usize>,
}

impl OverrideEdges {
    pub fn add(&mut self, edge: OverrideEdge) {
        match self.at.entry((edge.source, edge.target)) {
            Entry::Occupied(slot) => {
                let kept = &mut self.edges[*slot.get()];
                if edge.confidence > kept.confidence {
                    kept.confidence = edge.confidence;
                    kept.reason = edge.reason;
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(self.edges.len());
                self.edges.push(edge);
            }
        }
    }

    /// The edges in the order their pairs were first reached.
    pub fn into_edges(self) -> Vec<OverrideEdge> {
        self.edges
    }
}

/// Go: each embedded type that declares the name. No search beyond it.
fn embedded_namesakes<F: OverrideFacts>(
    facts: &F,
    matcher: Matcher<'_>,
    owner: u32,
    name: &str,
) -> Vec<Stop> {
    facts
        .declared_bases(owner)
        .iter()
        .filter_map(|base| bind(base.candidates()))
        .filter_map(|(ty, identity)| matcher.stop(ty, facts.methods(ty, name)?, identity, false))
        .collect()
}

/// Rust: the trait the method's `impl` block names (its `heritage`), never a
/// supertrait of it.
fn named_trait<F: OverrideFacts>(
    facts: &F,
    matcher: Matcher<'_>,
    owner: u32,
    name: &str,
    heritage: &[String],
) -> Vec<Stop> {
    facts
        .declared_bases(owner)
        .iter()
        .enumerate()
        .filter(|(_, base)| {
            let spelling = base.spelling().trim();
            !spelling.is_empty() && heritage.iter().any(|h| h.trim() == spelling)
        })
        .filter_map(|(index, base)| match bind(base.candidates()) {
            None => Some(Stop::Gap { owner, index }),
            Some((ty, identity)) => matcher.stop(ty, facts.methods(ty, name)?, identity, false),
        })
        .collect()
}

/// The lookup of one name through [`Chain`] orders, depth first.
struct Search<'a, F: OverrideFacts> {
    facts: &'a F,
    matcher: Matcher<'a>,
    chain: Chain,
    name: &'a str,
    stops: Vec<Stop>,
    /// The highest identity each searched type, reached through a generic
    /// base or not, was searched with, and whether its search settled the
    /// chain.
    seen: FxHashMap<(u32, bool), (f32, bool)>,
    /// The types from the method's own type to the current one.
    path: Vec<u32>,
}

impl<'a, F: OverrideFacts> Search<'a, F> {
    /// Searches the bases of `ty`. True when a chain base settled: found
    /// the name, or met a gap. `generic`: a base on the path so far was
    /// spelled with type arguments.
    fn bases(&mut self, ty: u32, identity: f32, generic: bool) -> bool {
        let facts = self.facts;
        let bases = facts.declared_bases(ty);
        let (chain, branches) = lookup_order(self.chain, facts, bases);
        let settled = chain
            .into_iter()
            .any(|index| self.base(ty, index, &bases[index], identity, generic));
        for index in branches {
            self.base(ty, index, &bases[index], identity, generic);
        }
        settled
    }

    fn base(
        &mut self,
        owner: u32,
        index: usize,
        base: &F::Base,
        identity: f32,
        generic: bool,
    ) -> bool {
        match bind(base.candidates()) {
            None => {
                self.stops.push(Stop::Gap { owner, index });
                true
            }
            // The weakest hop of the path is the identity of every edge
            // past it.
            Some((ty, confidence)) => self.visit(
                ty,
                identity.min(confidence),
                generic || base.spelling().contains('<'),
            ),
        }
    }

    fn visit(&mut self, ty: u32, identity: f32, generic: bool) -> bool {
        let why = if self.path.contains(&ty) {
            Some(Why::Cycle)
        } else if self.path.len() > MAX_OVERRIDE_DEPTH {
            Some(Why::Depth)
        } else {
            None
        };
        if let Some(why) = why {
            self.stops
                .push(Stop::Incomplete(Incomplete { at: ty, why }));
            return true;
        }
        let facts = self.facts;
        let stop = facts
            .methods(ty, self.name)
            .and_then(|decls| self.matcher.stop(ty, decls, identity, generic));
        if let Some(stop) = stop {
            self.stops.push(stop);
            return true;
        }
        if let Some(&(searched, settled)) = self.seen.get(&(ty, generic)) {
            if searched >= identity {
                return settled;
            }
        }
        self.path.push(ty);
        let settled = self.bases(ty, identity, generic);
        self.path.pop();
        self.seen.insert((ty, generic), (identity, settled));
        settled
    }
}

/// The declared bases of one type, by index, in lookup order: the chain,
/// searched until one base settles, then the branches, each searched on its
/// own. A blank spelling is no base.
fn lookup_order<F: OverrideFacts>(
    chain: Chain,
    facts: &F,
    bases: &[F::Base],
) -> (Vec<usize>, Vec<usize>) {
    let declared: Vec<usize> = (0..bases.len())
        .filter(|&i| !bases[i].spelling().trim().is_empty())
        .collect();
    match chain {
        Chain::PerBase => (Vec::new(), declared),
        // Ruby lists the superclass first and the mixins in include order;
        // `ancestors` reads the last mixin first and the superclass last.
        Chain::Ancestors => (declared.into_iter().rev().collect(), Vec::new()),
        // Dart lists `extends`, then `with`, then `implements`. A mixin
        // binds to a `Trait` (the parser's kind for `mixin`).
        Chain::Mixins => {
            let is_mixin = |&i: &usize| {
                bind(bases[i].candidates())
                    .is_some_and(|(id, _)| facts.node_kind(id) == NodeKind::Trait)
            };
            match (
                declared.iter().position(is_mixin),
                declared.iter().rposition(is_mixin),
            ) {
                (Some(first), Some(last)) => {
                    let mut chain: Vec<usize> =
                        declared[first..=last].iter().rev().copied().collect();
                    chain.extend_from_slice(&declared[..first]);
                    (chain, declared[last + 1..].to_vec())
                }
                _ => (Vec::new(), declared),
            }
        }
    }
}

/// Python: the first entry after the type in its C3 order that declares an
/// applicable method of the name, or is a gap.
fn mro_lookup<F: OverrideFacts>(
    facts: &F,
    matcher: Matcher<'_>,
    owner: u32,
    name: &str,
    mros: &mut MroCache,
) -> Vec<Stop> {
    let order = match mro(facts, owner, &mut mros.0, &mut Vec::new()) {
        Ok(order) => order,
        Err(incomplete) => return vec![Stop::Incomplete(incomplete)],
    };
    for entry in order.into_iter().skip(1) {
        match entry {
            MroEntry::Gap {
                owner: declaring,
                index,
            } => {
                return vec![Stop::Gap {
                    owner: declaring,
                    index,
                }]
            }
            MroEntry::Type(ty) => {
                let Some(decls) = facts.methods(ty, name) else {
                    continue;
                };
                let identity = widest_identity(facts, owner, ty, &mut FxHashMap::default());
                if let Some(stop) = matcher.stop(ty, decls, identity, false) {
                    return vec![stop];
                }
            }
        }
    }
    Vec::new()
}

/// The C3 linearization of `ty`, memoized in `memo`. `path` holds the types
/// whose order is being computed.
fn mro<F: OverrideFacts>(
    facts: &F,
    ty: u32,
    memo: &mut FxHashMap<u32, Mro>,
    path: &mut Vec<u32>,
) -> Mro {
    if let Some(known) = memo.get(&ty) {
        return known.clone();
    }
    let why = if path.contains(&ty) {
        Some(Why::Cycle)
    } else if path.len() > MAX_OVERRIDE_DEPTH {
        Some(Why::Depth)
    } else {
        None
    };
    if let Some(why) = why {
        return Err(Incomplete { at: ty, why });
    }
    path.push(ty);
    let order = mro_of_bases(facts, ty, memo, path);
    path.pop();
    memo.insert(ty, order.clone());
    order
}

fn mro_of_bases<F: OverrideFacts>(
    facts: &F,
    ty: u32,
    memo: &mut FxHashMap<u32, Mro>,
    path: &mut Vec<u32>,
) -> Mro {
    let mut sequences: Vec<Vec<MroEntry>> = Vec::new();
    let mut heads: Vec<MroEntry> = Vec::new();
    for (index, base) in facts.declared_bases(ty).iter().enumerate() {
        if base.spelling().trim().is_empty() {
            continue;
        }
        let head = match bind(base.candidates()) {
            Some((id, _)) => {
                sequences.push(mro(facts, id, memo, path)?);
                MroEntry::Type(id)
            }
            None => {
                let gap = MroEntry::Gap { owner: ty, index };
                sequences.push(vec![gap]);
                gap
            }
        };
        heads.push(head);
    }
    sequences.push(heads);
    let mut order = c3_merge(&sequences).ok_or(Incomplete {
        at: ty,
        why: Why::Inconsistent,
    })?;
    order.insert(0, MroEntry::Type(ty));
    Ok(order)
}

/// The C3 merge: repeatedly take the first head that is in no sequence's
/// tail. `None` when no head qualifies (Python's `TypeError`).
fn c3_merge(sequences: &[Vec<MroEntry>]) -> Option<Vec<MroEntry>> {
    let mut starts = vec![0usize; sequences.len()];
    let mut out = Vec::new();
    loop {
        let live: Vec<usize> = (0..sequences.len())
            .filter(|&i| starts[i] < sequences[i].len())
            .collect();
        if live.is_empty() {
            return Some(out);
        }
        let head = live.iter().map(|&i| sequences[i][starts[i]]).find(|head| {
            live.iter()
                .all(|&j| !sequences[j][starts[j] + 1..].contains(head))
        })?;
        out.push(head);
        for &i in &live {
            if sequences[i][starts[i]] == head {
                starts[i] += 1;
            }
        }
    }
}

/// The highest identity over the bound paths from `from` to `to`, where a
/// path's identity is its weakest resolver confidence.
fn widest_identity<F: OverrideFacts>(
    facts: &F,
    from: u32,
    to: u32,
    memo: &mut FxHashMap<u32, f32>,
) -> f32 {
    if from == to {
        return 1.0;
    }
    if let Some(&known) = memo.get(&from) {
        return known;
    }
    memo.insert(from, 0.0);
    let mut best = 0.0f32;
    for base in facts.declared_bases(from) {
        if let Some((id, confidence)) = bind(base.candidates()) {
            best = best.max(confidence.min(widest_identity(facts, id, to, memo)));
        }
    }
    memo.insert(from, best);
    best
}

/// Per node, the index of its owner type (module doc) in `nodes`, or
/// `None`.
///
/// One sweep over the types and callables in source order keeps the open
/// containers on a stack, so a file costs O(n log n), not one scan of every
/// container per callable. Spans of one tree nest or are disjoint; between
/// two identical spans the first stays the container.
pub fn owner_types<N: OverrideNode>(nodes: &[N]) -> Vec<Option<usize>> {
    let is_container = |i: usize| holds_heritage(nodes[i].kind()) || nodes[i].kind().is_callable();
    let mut order: Vec<usize> = (0..nodes.len()).filter(|&i| is_container(i)).collect();
    order.sort_by_key(|&i| {
        let (r1, c1, r2, c2) = nodes[i].span();
        ((r1, c1), Reverse((r2, c2)), i)
    });
    let mut innermost: Vec<Option<usize>> = vec![None; nodes.len()];
    let mut stack: Vec<usize> = Vec::new();
    for &i in &order {
        let span = nodes[i].span();
        while let Some(&top) = stack.last() {
            if span_contains(nodes[top].span(), span) {
                break;
            }
            stack.pop();
        }
        match stack.last() {
            Some(&top) if nodes[top].span() == span => innermost[i] = innermost[top],
            top => {
                innermost[i] = top.copied();
                stack.push(i);
            }
        }
    }
    let mut types_by_name: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
    for (i, n) in nodes.iter().enumerate() {
        if holds_heritage(n.kind()) {
            types_by_name.entry(n.name()).or_default().push(i);
        }
    }
    (0..nodes.len())
        .map(|i| {
            if !nodes[i].kind().is_callable() {
                return None;
            }
            match innermost[i] {
                // A function inside a callable is a local, not a member.
                Some(j) => holds_heritage(nodes[j].kind()).then_some(j),
                None => {
                    let owner = nodes[i].owner_class()?.trim();
                    match types_by_name.get(owner)?.as_slice() {
                        [only] => Some(*only),
                        _ => None,
                    }
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_override_marker_each_language_spelling_returns_true() {
        for marker in [
            "__override__",
            "@Override",
            "@java.lang.Override",
            "@override",
            "override",
            "typing.override",
        ] {
            assert!(is_override_marker(marker), "{marker}");
        }
        assert!(!is_override_marker("@overrides"));
        assert!(!is_override_marker("@Deprecated"));
    }

    fn ty(id: u32) -> MroEntry {
        MroEntry::Type(id)
    }

    /// Stands for: Python's C3 order, which the MRO lookup walks.
    #[test]
    fn test_c3_merge_diamond_returns_python_order() {
        // class A; class B(A); class C(A); class D(B, C)
        let a = vec![ty(0)];
        let b = vec![ty(1), ty(0)];
        let c = vec![ty(2), ty(0)];
        assert_eq!(c3_merge(&[a]), Some(vec![ty(0)]));
        assert_eq!(
            c3_merge(&[b, c, vec![ty(1), ty(2)]]),
            Some(vec![ty(1), ty(2), ty(0)]),
            "D's order after D: B, C, A"
        );
    }

    /// Stands for: an MRO Python rejects ends the lookup as incomplete.
    #[test]
    fn test_c3_merge_inconsistent_order_returns_none() {
        // class X(A, B); class Y(B, A); class Z(X, Y)
        let x = vec![ty(2), ty(0), ty(1)];
        let y = vec![ty(3), ty(1), ty(0)];
        assert_eq!(c3_merge(&[x, y, vec![ty(2), ty(3)]]), None);
    }

    /// Stands for: an unbound base keeps its declared slot in the order, so
    /// a later base is never reached past it.
    #[test]
    fn test_c3_merge_gap_listed_first_precedes_later_base() {
        let gap = MroEntry::Gap { owner: 9, index: 0 };
        let b = vec![ty(1), ty(0)];
        assert_eq!(
            c3_merge(&[vec![gap], b, vec![gap, ty(1)]]),
            Some(vec![gap, ty(1), ty(0)])
        );
    }

    /// Stands for: the confidence classes a machine reader tells apart by
    /// band and `reason` (C4).
    #[test]
    fn test_evidence_identity_bands_return_class_and_reason() {
        assert_eq!(
            evidence(true, 1.0),
            (1.0, OVERRIDES_REASON),
            "same-file marked"
        );
        assert_eq!(
            evidence(true, 0.95),
            (0.95, OVERRIDES_REASON),
            "an imported base carries its confidence, a marker does not raise it"
        );
        assert_eq!(
            evidence(false, 1.0),
            (NAME_MATCH_CONFIDENCE, OVERRIDES_NAME_MATCH_REASON)
        );
        assert_eq!(
            evidence(true, HEURISTIC_CEILING),
            (NAME_MATCH_CONFIDENCE, OVERRIDES_BASE_BY_NAME_REASON),
            "a bare-name base is never Exact, even when marked"
        );
        assert_eq!(
            evidence(false, HEURISTIC_CEILING),
            (NAME_MATCH_CONFIDENCE, OVERRIDES_BASE_BY_NAME_REASON)
        );
    }

    fn meta(flags: u16, params: &[&str]) -> RawFunctionMeta {
        RawFunctionMeta {
            span: (0, 0, 0, 0),
            flags,
            params: params.iter().map(|p| p.to_string()).collect(),
            return_type: String::new(),
            decorators: Vec::new(),
        }
    }

    fn matcher(
        signature: Signature,
        statics: Statics,
        own: Option<&RawFunctionMeta>,
    ) -> Matcher<'_> {
        Matcher {
            own,
            signature,
            statics,
        }
    }

    const TYPED: Signature = Signature::Typed {
        virtual_only: false,
        nullable_annotations: false,
    };

    /// Stands for: in an overloading language the signature picks the
    /// overridden declaration; an overload or a static is skipped.
    #[test]
    fn test_fit_typed_signature_skips_overload_and_static() {
        let own = meta(0, &["n", "int"]);
        let m = matcher(TYPED, Statics::Never, Some(&own));
        assert_eq!(
            m.fit(Some(&meta(0, &["x", " int "])), false),
            Fit::Match { confirmed: true },
            "whitespace is no difference"
        );
        assert_eq!(
            m.fit(Some(&meta(0, &[])), false),
            Fit::Skip,
            "another arity"
        );
        assert_eq!(
            m.fit(Some(&meta(0, &["s", "String"])), false),
            Fit::Skip,
            "another type"
        );
        assert_eq!(
            m.fit(Some(&meta(FunctionMeta::FLAG_STATIC, &["n", "int"])), false),
            Fit::Skip,
            "a static is no target"
        );
        let own_static = meta(FunctionMeta::FLAG_STATIC, &["n", "int"]);
        assert!(matcher(TYPED, Statics::Never, Some(&own_static)).skips_method());
        assert!(!matcher(TYPED, Statics::Paired, Some(&own_static)).skips_method());
    }

    /// Stands for: a typed-language pair the meta cannot compare ends the
    /// path with no edge, never a guessed `Exact` one.
    #[test]
    fn test_fit_typed_missing_meta_or_blank_type_returns_not_comparable() {
        let own = meta(0, &["cb", ""]);
        let m = matcher(TYPED, Statics::Never, Some(&own));
        let not_comparable = Fit::Unknown(Ambiguity::NotComparable);
        assert_eq!(m.fit(None, false), not_comparable, "base without meta");
        assert_eq!(
            matcher(TYPED, Statics::Never, None).fit(Some(&meta(0, &[])), false),
            not_comparable,
            "own method without meta"
        );
        assert_eq!(m.fit(Some(&meta(0, &["cb", ""])), false), not_comparable);
        let typed = meta(0, &["n", "T"]);
        let own_typed = meta(0, &["n", "String"]);
        let m = matcher(TYPED, Statics::Never, Some(&own_typed));
        assert_eq!(m.fit(Some(&typed), false), Fit::Skip);
        assert_eq!(
            m.fit(Some(&typed), true),
            not_comparable,
            "through a base with type arguments, `T` may stand for `String`"
        );
    }

    /// Stands for: C# parameter types that differ only in `?` may be one
    /// reference type or two value types, so the pair is undecided; a real
    /// type difference beside it still skips. Kotlin keeps `?` in the type.
    #[test]
    fn test_fit_csharp_nullable_annotation_only_returns_nullable() {
        let csharp = Signature::Typed {
            virtual_only: false,
            nullable_annotations: true,
        };
        let own = meta(0, &["s", "string", "xs", "List<int>"]);
        let m = matcher(csharp, Statics::Never, Some(&own));
        assert_eq!(
            m.fit(Some(&meta(0, &["s", "string?", "xs", "List<int?>"])), false),
            Fit::Unknown(Ambiguity::Nullable)
        );
        assert_eq!(
            m.fit(Some(&meta(0, &["s", "string?", "xs", "List<long>"])), false),
            Fit::Skip,
            "a real type difference wins"
        );
        assert_eq!(
            m.fit(Some(&meta(0, &["s", "string", "xs", "List<int>"])), false),
            Fit::Match { confirmed: true }
        );
        let kotlin = matcher(TYPED, Statics::Never, Some(&own));
        assert_eq!(
            kotlin.fit(Some(&meta(0, &["s", "string?", "xs", "List<int>"])), false),
            Fit::Skip
        );
    }

    /// Stands for: C++ binds a base method only when it is virtual.
    #[test]
    fn test_fit_cpp_non_virtual_base_returns_not_virtual() {
        let own = meta(FunctionMeta::FLAG_VIRTUAL, &[]);
        let m = matcher(
            Signature::Typed {
                virtual_only: true,
                nullable_annotations: false,
            },
            Statics::Never,
            Some(&own),
        );
        assert_eq!(
            m.fit(Some(&meta(FunctionMeta::FLAG_VIRTUAL, &[])), false),
            Fit::Match { confirmed: true }
        );
        assert_eq!(
            m.fit(Some(&meta(0, &[])), false),
            Fit::Unknown(Ambiguity::NotVirtual)
        );
    }

    /// Stands for: without overloading, arity confirms a pair; a difference
    /// is reported where the compiler checks the override, and pairs by
    /// name alone where the meta keeps no default value.
    #[test]
    fn test_fit_arity_and_lenient_arity_difference() {
        let own = meta(0, &["x", ""]);
        let none = meta(0, &[]);
        let rest = meta(0, &["...xs", ""]);
        let arity = matcher(Signature::Arity, Statics::Paired, Some(&own));
        assert_eq!(
            arity.fit(Some(&meta(0, &["y", "number"])), false),
            Fit::Match { confirmed: true }
        );
        assert_eq!(
            arity.fit(Some(&none), false),
            Fit::Unknown(Ambiguity::Arity { base: 0, own: 1 })
        );
        // A rest parameter absorbs an arity difference, so the pair holds
        // by name alone.
        let own_two = meta(0, &["x", "", "y", ""]);
        let arity_two = matcher(Signature::Arity, Statics::Paired, Some(&own_two));
        assert_eq!(
            arity_two.fit(Some(&rest), false),
            Fit::Match { confirmed: false }
        );
        assert_eq!(arity.fit(None, false), Fit::Match { confirmed: false });
        let lenient = matcher(Signature::Lenient, Statics::Paired, Some(&own));
        assert_eq!(
            lenient.fit(Some(&none), false),
            Fit::Match { confirmed: false }
        );
        let own_static = meta(FunctionMeta::FLAG_STATIC, &["x", ""]);
        let paired = matcher(Signature::Lenient, Statics::Paired, Some(&own_static));
        assert_eq!(
            paired.fit(Some(&own), false),
            Fit::Skip,
            "static against instance"
        );
        assert_eq!(
            paired.fit(Some(&own_static), false),
            Fit::Match { confirmed: true },
            "a class-side method overrides a class-side one"
        );
    }

    fn decl(slot: u32, meta: &RawFunctionMeta) -> Decl<'_> {
        Decl {
            slot,
            meta: Some(meta),
        }
    }

    /// Stands for: several applicable declarations in the nearest type are
    /// a reported ambiguity; an overload among them is not counted.
    #[test]
    fn test_matcher_stop_counts_applicable_declarations_only() {
        let own = meta(0, &[]);
        let (zero, one) = (meta(0, &[]), meta(0, &["n", "int"]));
        let m = matcher(TYPED, Statics::Never, Some(&own));
        assert!(matches!(
            m.stop(7, &[decl(1, &zero), decl(2, &one)], 0.95, false),
            Some(Stop::Found {
                ty: 7,
                target: 1,
                confirmed: true,
                ..
            })
        ));
        assert!(matches!(
            m.stop(7, &[decl(1, &zero), decl(2, &zero)], 1.0, false),
            Some(Stop::Ambiguous {
                why: Ambiguity::Several(2),
                ..
            })
        ));
        assert!(
            m.stop(7, &[decl(2, &one)], 1.0, false).is_none(),
            "an overload only: go on"
        );
    }

    /// Stands for: a base the resolver picked by path proximity alone, or
    /// one with several candidates, binds no type.
    #[test]
    fn test_bind_nearest_copy_and_several_return_none() {
        assert_eq!(bind(&[(1, 1.0)]), Some((1, 1.0)));
        assert_eq!(bind(&[(1, 0.7)]), Some((1, 0.7)));
        assert_eq!(bind(&[(1, NEAREST_COPY_CONFIDENCE)]), None);
        assert_eq!(bind(&[(1, 1.0), (2, 1.0)]), None);
        assert_eq!(bind(&[]), None);
    }

    /// A hand-made hierarchy: per type its bases, kinds and methods.
    #[derive(Default)]
    struct Hierarchy<'a> {
        bases: FxHashMap<u32, Vec<BaseRow>>,
        kinds: FxHashMap<u32, NodeKind>,
        methods: MethodIndex<'a>,
    }

    impl<'a> Hierarchy<'a> {
        fn base(mut self, ty: u32, spelling: &str, candidates: &[(u32, f32)]) -> Self {
            self.bases.entry(ty).or_default().push(BaseRow {
                spelling: spelling.to_string(),
                candidates: candidates.to_vec(),
            });
            self
        }

        fn method(mut self, ty: u32, name: &'a str, slot: u32, meta: &'a RawFunctionMeta) -> Self {
            self.methods
                .entry((ty, name))
                .or_default()
                .push(decl(slot, meta));
            self
        }
    }

    impl OverrideFacts for Hierarchy<'_> {
        type Base = BaseRow;

        fn declared_bases(&self, ty: u32) -> &[BaseRow] {
            self.bases.get(&ty).map(Vec::as_slice).unwrap_or_default()
        }

        fn partial_type(&self, ty: u32) -> u32 {
            ty
        }

        fn node_kind(&self, id: u32) -> NodeKind {
            self.kinds.get(&id).copied().unwrap_or(NodeKind::Class)
        }

        fn methods<'s>(&'s self, ty: u32, name: &'s str) -> Option<&'s [Decl<'s>]> {
            self.methods.get(&(ty, name)).map(Vec::as_slice)
        }
    }

    fn method<'a>(
        name: &'a str,
        decorators: &'a [String],
        meta: &'a RawFunctionMeta,
    ) -> OverrideMethod<'a> {
        OverrideMethod {
            name,
            kind: NodeKind::Method,
            decorators,
            heritage: &[],
            meta: Some(meta),
        }
    }

    /// Every edge `owner`'s method `run` at `source` gets.
    fn edges_of<F: OverrideFacts>(
        facts: &F,
        language: Language,
        run: &OverrideMethod<'_>,
        owner: u32,
        source: u32,
    ) -> Vec<OverrideEdge> {
        let rules = rules_for(language).expect("the language has rules");
        let mut edges = OverrideEdges::default();
        if let Some(lookup) = method_lookup(facts, rules, run, owner, &mut MroCache::default()) {
            for &stop in &lookup.stops {
                if let Some(edge) = lookup.edge(source, stop) {
                    edges.add(edge);
                }
            }
        }
        edges.into_edges()
    }

    /// Stands for: the Python lookup follows C3, so in `class D(B, C)` with
    /// `B(A)`, `C(A)` and only `A` and `C` declaring `run`, `D.run` overrides
    /// `C.run`, not `A.run` (a depth-first walk reaches `A` through `B`
    /// first).
    #[test]
    fn test_method_lookup_python_diamond_returns_c3_next_declaration() {
        let (a, b, c, d) = (0, 1, 2, 3);
        let one = meta(0, &["self", ""]);
        let facts = Hierarchy::default()
            .base(b, "A", &[(a, 1.0)])
            .base(c, "A", &[(a, 1.0)])
            .base(d, "B", &[(b, 1.0)])
            .base(d, "C", &[(c, 1.0)])
            .method(a, "run", 10, &one)
            .method(c, "run", 12, &one);
        let edges = edges_of(&facts, Language::Python, &method("run", &[], &one), d, 13);
        assert_eq!(
            edges,
            [OverrideEdge {
                source: 13,
                target: 12,
                confidence: NAME_MATCH_CONFIDENCE,
                reason: OVERRIDES_NAME_MATCH_REASON,
            }],
            "unmarked: by name alone, at the name-match confidence"
        );
        let marked = vec!["@override".to_string()];
        let edges = edges_of(
            &facts,
            Language::Python,
            &method("run", &marked, &one),
            d,
            13,
        );
        assert_eq!(
            edges
                .iter()
                .map(|e| (e.target, e.confidence, e.reason))
                .collect::<Vec<_>>(),
            [(12, 1.0, OVERRIDES_REASON)],
            "marked, through same-file bases: Exact at 1.0"
        );
    }

    /// Stands for: per base, Java gives the class chain and each interface
    /// an edge of its own, at the weakest confidence of its path; a static
    /// method takes no part; the language with no override concept gets no
    /// edge.
    #[test]
    fn test_method_lookup_per_base_static_and_no_concept() {
        let (base, iface, child) = (0, 1, 2);
        let one = meta(0, &["n", "int"]);
        let statik = meta(FunctionMeta::FLAG_STATIC, &["n", "int"]);
        let marked = vec!["@Override".to_string()];
        let facts = Hierarchy::default()
            .base(child, "Base", &[(base, 0.95)])
            .base(child, "Api", &[(iface, 1.0)])
            .method(base, "run", 10, &one)
            .method(iface, "run", 11, &one);
        let run = method("run", &marked, &one);
        let edges = edges_of(&facts, Language::Java, &run, child, 12);
        assert_eq!(
            edges,
            [
                OverrideEdge {
                    source: 12,
                    target: 10,
                    confidence: 0.95,
                    reason: OVERRIDES_REASON,
                },
                OverrideEdge {
                    source: 12,
                    target: 11,
                    confidence: 1.0,
                    reason: OVERRIDES_REASON,
                },
            ]
        );
        assert!(
            edges_of(&facts, Language::Java, &method("run", &[], &one), child, 12).is_empty(),
            "Java pairs only a marked method"
        );
        assert!(
            edges_of(
                &facts,
                Language::Java,
                &method("run", &marked, &statik),
                child,
                12
            )
            .is_empty(),
            "a static method overrides nothing"
        );
        assert!(
            edges_of(&facts, Language::Go, &run, child, 12).is_empty(),
            "Go has no override"
        );
        assert!(rules_for(Language::C).is_none(), "C has no override");
    }

    /// Stands for: the same pair reached on two paths is one edge with the
    /// higher confidence, at its first position.
    #[test]
    fn test_override_edges_same_pair_keeps_higher_confidence() {
        let mut edges = OverrideEdges::default();
        let edge = |target: u32, confidence: f32, reason: &'static str| OverrideEdge {
            source: 1,
            target,
            confidence,
            reason,
        };
        edges.add(edge(
            2,
            NAME_MATCH_CONFIDENCE,
            OVERRIDES_BASE_BY_NAME_REASON,
        ));
        edges.add(edge(3, 1.0, OVERRIDES_REASON));
        edges.add(edge(2, 0.95, OVERRIDES_REASON));
        edges.add(edge(2, 0.8, OVERRIDES_REASON));
        assert_eq!(
            edges.into_edges(),
            [
                edge(2, 0.95, OVERRIDES_REASON),
                edge(3, 1.0, OVERRIDES_REASON)
            ]
        );
    }

    /// Stands for: a whitespace-only name and a non-callable take no part,
    /// an owner without a declared base has no lookup, and a padded name is
    /// trimmed.
    #[test]
    fn test_method_lookup_blank_name_or_no_base_returns_none() {
        let one = meta(0, &["self", ""]);
        let facts = Hierarchy::default()
            .base(1, "A", &[(0, 1.0)])
            .method(0, "run", 10, &one);
        let rules = rules_for(Language::Python).expect("python");
        let mut mros = MroCache::default();
        assert!(method_lookup(&facts, rules, &method("  ", &[], &one), 1, &mut mros).is_none());
        let mut class = method("run", &[], &one);
        class.kind = NodeKind::Class;
        assert!(method_lookup(&facts, rules, &class, 1, &mut mros).is_none());
        assert!(
            method_lookup(&facts, rules, &method("run", &[], &one), 0, &mut mros).is_none(),
            "A has no base"
        );
        assert!(
            method_lookup(&facts, rules, &method(" run ", &[], &one), 1, &mut mros)
                .is_some_and(|lookup| lookup.stops.len() == 1)
        );
    }
}
