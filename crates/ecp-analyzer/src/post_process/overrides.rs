//! `Overrides` edge emission: a method → the nearest applicable declaration
//! of its name along the declared bases of its type.
//!
//! # The declared base, not a same-named type
//!
//! The owner type of a method is the innermost enclosing type that holds
//! heritage ([`holds_heritage`], the kinds `enclosing_class_heritage` reads),
//! when no callable sits between them: a local function is no member. A
//! method declared outside its type (a Go receiver method, a Rust `impl`
//! block member, a C++ out-of-line definition) takes the one type of its
//! file that its `owner_class` names. The bases of that type are the ones
//! Pass 1.9 resolved through the type's imports and scope
//! (`SymbolTable::declared_bases`), so another type that only shares a
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
//! | `Marked` | Java, Kotlin, C#, C++, others | a method the parser marked (`@Override`, `override`) | 1.0, `Exact` |
//! | `Inferred` | TypeScript, JavaScript, Python, PHP, Ruby, Swift, Dart | a marked method, else any method of a type with bases | marked: 1.0; unmarked: [`NAME_MATCH_CONFIDENCE`], `Heuristic` |
//! | `TraitImpl` | Rust | a method of an `impl Trait for T` block, which carries the trait as heritage | 1.0, `Exact`, only into that trait, never a supertrait |
//! | `NoConcept` | Go | none | no edge; a [`BS_KIND_NO_OVERRIDE_CONCEPT`] BlindSpot where a method shadows one of an embedded type |
//!
//! C has no override and the pass emits nothing for it. The parsers of the
//! `Inferred` languages capture no mandatory marker, so an unmarked method
//! there pairs with its base by name alone, and the edge stays below
//! `HEURISTIC_CEILING` with its own `reason` (C4 evidence classes).
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
//! The same declaration reached twice (a diamond) is one edge. Distance does
//! not lower the confidence; the weakest resolver confidence on the path
//! does ([`evidence`]).
//!
//! # Applicable declaration
//!
//! A declaration of the name applies when its signature, read from the
//! function meta paired by span, makes it the overridden one
//! ([`Matcher::fit`]). An overload or a static is skipped and the lookup
//! goes on past it; an undecided one ends the path with no edge:
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
//! path with a BlindSpot at the method, deduplicated per method and base:
//!
//! | Kind | Gap |
//! |---|---|
//! | [`BS_KIND_OVERRIDE_BASE_UNRESOLVED`] | a declared base resolved to no project type (external, or a resolution failure) |
//! | [`BS_KIND_OVERRIDE_BASE_AMBIGUOUS`] | a declared base names several project types, or one picked by path proximity alone |
//! | [`BS_KIND_OVERRIDE_AMBIGUOUS`] | the nearest declaring base has several applicable declarations, or one the signature leaves undecided (table above) |
//! | [`BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE`] | a heritage cycle, more than [`MAX_OVERRIDE_DEPTH`] levels, or no consistent Python MRO |

use crate::resolution::index::{DeclaredBase, FileMeta, Language, ResolveTarget, SymbolTable};
use ecp_core::analyzer::heritage::holds_heritage;
use ecp_core::analyzer::types::{
    span_contains, LocalGraph, RawFunctionMeta, RawNode, NEAREST_COPY_CONFIDENCE,
};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap, HEURISTIC_CEILING};
use ecp_core::graph::{BlindSpotRecord, Edge, File, FileCategory, FunctionMeta, NodeKind, RelType};
use ecp_core::pool::{StrRef, StringPool};
use rustc_hash::{FxHashMap, FxHashSet};
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

/// Every BlindSpot kind this pass records.
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

const EXACT_REASON: &str = "post_process:overrides";
const NAME_MATCH_REASON: &str = "post_process:overrides:name-in-base";
const BASE_BY_NAME_REASON: &str = "post_process:overrides:base-by-name";

/// Sentinel written by per-language parsers when the `override` keyword /
/// annotation is present. Java writes `@Override` (the literal annotation
/// text); Kotlin / C# / C++ / Swift write this sentinel.
const OVERRIDE_SENTINEL: &str = "__override__";

/// One declaration of a method name in a type: its slot, and the function
/// meta the parser paired with it by span (as Pass 1.8 does).
#[derive(Debug, Clone, Copy)]
struct Decl<'a> {
    slot: u32,
    meta: Option<&'a RawFunctionMeta>,
}

/// `(type slot, method name)` → the methods so named that the type declares.
type MethodIndex<'a> = FxHashMap<(u32, &'a str), Vec<Decl<'a>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverrideRule {
    Marked,
    Inferred,
    TraitImpl,
    NoConcept,
}

/// The order a language searches the bases of one type ([`lookup_order`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chain {
    PerBase,
    Ancestors,
    Mixins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    Chain(Chain),
    Mro,
}

/// How a language tells the declaration a method overrides from an
/// overload of it ([`Matcher::fit`]).
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

#[derive(Debug, Clone, Copy)]
struct Rules {
    rule: OverrideRule,
    lookup: Lookup,
    signature: Signature,
    statics: Statics,
}

/// The rules of the file at `path`; `None` for C, which has no override.
fn rules_for(path: &str) -> Option<Rules> {
    use OverrideRule::*;
    use Signature::*;
    let per_base = Lookup::Chain(Chain::PerBase);
    let typed = Typed {
        virtual_only: false,
        nullable_annotations: false,
    };
    let (rule, lookup, signature, statics) = match Language::from_path(path) {
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
        Language::Python => (Inferred, Lookup::Mro, Lenient, Statics::Paired),
        Language::Ruby => (
            Inferred,
            Lookup::Chain(Chain::Ancestors),
            Lenient,
            Statics::Paired,
        ),
        Language::Dart => (
            Inferred,
            Lookup::Chain(Chain::Mixins),
            Arity,
            Statics::Never,
        ),
        Language::Rust => (TraitImpl, per_base, Unchecked, Statics::Ignored),
        Language::Go => (NoConcept, per_base, Unchecked, Statics::Ignored),
        Language::C => return None,
        _ => (Marked, per_base, Unchecked, Statics::Ignored),
    };
    Some(Rules {
        rule,
        lookup,
        signature,
        statics,
    })
}

/// Why the nearest declaring type gives no edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ambiguity {
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
    fn stop<'b>(
        &self,
        ty: u32,
        decls: &[Decl<'_>],
        identity: f32,
        generic: bool,
    ) -> Option<Stop<'b>> {
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
fn is_override_marker(decorator: &str) -> bool {
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

fn is_marked(raw_node: &RawNode) -> bool {
    raw_node.decorators.iter().any(|d| is_override_marker(d))
}

/// Why a lookup stopped short of the whole hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    Cycle,
    Depth,
    Inconsistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Incomplete {
    /// The type where the lookup stopped.
    at: u32,
    why: Why,
}

/// Where the lookup of one name ended on one path.
#[derive(Debug, Clone, Copy)]
enum Stop<'a> {
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
    /// Declared base `index` of `owner` binds no one project type.
    Gap {
        owner: u32,
        index: usize,
        base: &'a DeclaredBase,
    },
    Incomplete(Incomplete),
}

/// The one project type `base` binds and the resolver's confidence. `None`
/// for no candidate, several, or one picked by path proximity alone.
fn bind(base: &DeclaredBase) -> Option<(u32, f32)> {
    match *base.candidates {
        [(id, confidence)] if confidence > NEAREST_COPY_CONFIDENCE => Some((id, confidence)),
        _ => None,
    }
}

/// The interned `reason` of each edge class.
struct Reasons {
    exact: StrRef,
    name: StrRef,
    base_by_name: StrRef,
}

/// The confidence and reason of an edge whose path has `identity`.
///
/// A base bound by a unique bare name (the `Global` tier, at the ceiling)
/// never yields an `Exact` edge, even when marked. A base bound through an
/// import or a qualifier carries the resolver's confidence: a marker does
/// not raise it to 1.0.
fn evidence(exact_rule: bool, identity: f32, reasons: &Reasons) -> (f32, StrRef) {
    if identity <= HEURISTIC_CEILING {
        (NAME_MATCH_CONFIDENCE, reasons.base_by_name)
    } else if exact_rule {
        (identity.min(1.0), reasons.exact)
    } else {
        (NAME_MATCH_CONFIDENCE, reasons.name)
    }
}

/// Emit `Overrides` edges for all languages, and the BlindSpots of the
/// module doc.
///
/// Requires the declared bases of Pass 1.9 in `symbol_table`. Returns the
/// number of edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    files: &[File],
    raw_map: &RawIndexMap,
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
    blind_spots: &mut Vec<BlindSpotRecord>,
) -> usize {
    let owners: Vec<Vec<Option<usize>>> = local_graphs
        .iter()
        .map(|lg| owner_types(&lg.nodes))
        .collect();
    let metas: Vec<Vec<Option<&RawFunctionMeta>>> =
        local_graphs.iter().map(metas_by_node).collect();
    let methods = build_method_index(local_graphs, &owners, &metas, raw_map, symbol_table);
    let reasons = Reasons {
        exact: string_pool.add(EXACT_REASON),
        name: string_pool.add(NAME_MATCH_REASON),
        base_by_name: string_pool.add(BASE_BY_NAME_REASON),
    };
    let name_of = |slot: u32| slot_name(local_graphs, raw_map, slot).unwrap_or("");

    let mut emitted: FxHashMap<(u32, u32), usize> = FxHashMap::default();
    let mut reported: FxHashSet<(u32, u8, u32, u32)> = FxHashSet::default();
    let mut mros: FxHashMap<u32, Mro> = FxHashMap::default();
    for (file_idx, lg) in local_graphs.iter().enumerate() {
        let file = FileOrd(file_idx as u32);
        let path = lg.file_path.to_string_lossy().replace('\\', "/");
        let Some(Rules {
            rule,
            lookup,
            signature,
            statics,
        }) = rules_for(&path)
        else {
            continue;
        };
        let is_test = files
            .get(file_idx)
            .is_some_and(|f| matches!(f.category, FileCategory::Test));
        for (raw_idx, node) in lg.nodes.iter().enumerate() {
            let name = node.name.trim();
            if !node.kind.is_callable() || name.is_empty() {
                continue;
            }
            let marked = is_marked(node);
            let exact_rule = match rule {
                OverrideRule::Marked if !marked => continue,
                OverrideRule::Inferred if !marked && node.kind == NodeKind::Constructor => continue,
                OverrideRule::TraitImpl if node.heritage.is_empty() => continue,
                OverrideRule::Marked | OverrideRule::TraitImpl => true,
                OverrideRule::Inferred => marked,
                OverrideRule::NoConcept => false,
            };
            let matcher = Matcher {
                own: metas[file_idx][raw_idx],
                signature,
                statics,
            };
            if matcher.skips_method() {
                continue;
            }
            let Some(owner) = owners[file_idx][raw_idx] else {
                continue;
            };
            let (Some(owner_slot), Some(source)) = (
                raw_map.node(file, RawIdx(owner as u32)),
                raw_map.node(file, RawIdx(raw_idx as u32)),
            ) else {
                continue;
            };
            if symbol_table.declared_bases(owner_slot).is_empty() {
                continue;
            }
            let stops = match (rule, lookup) {
                (OverrideRule::NoConcept, _) => {
                    embedded_namesakes(symbol_table, &methods, matcher, owner_slot, name)
                }
                (OverrideRule::TraitImpl, _) => named_trait(
                    symbol_table,
                    &methods,
                    matcher,
                    owner_slot,
                    name,
                    &node.heritage,
                ),
                (_, Lookup::Mro) => {
                    mro_lookup(symbol_table, &methods, matcher, owner_slot, name, &mut mros)
                }
                (_, Lookup::Chain(chain)) => {
                    let mut search = Search {
                        table: symbol_table,
                        methods: &methods,
                        matcher,
                        chain,
                        name,
                        stops: Vec::new(),
                        seen: FxHashMap::default(),
                        path: vec![symbol_table.partial_type(owner_slot)],
                    };
                    search.bases(owner_slot, 1.0, false);
                    search.stops
                }
            };
            for stop in stops {
                let (key, kind, hint) = match stop {
                    Stop::Found { target, .. } if target == source => continue,
                    Stop::Found { ty, .. } if rule == OverrideRule::NoConcept => (
                        (source, 3, ty, 0),
                        BS_KIND_NO_OVERRIDE_CONCEPT,
                        format!(
                            "`{name}` shadows the `{name}` of the embedded `{}`; the language has no method override, so no Overrides edge",
                            name_of(ty)
                        ),
                    ),
                    Stop::Found {
                        target,
                        identity,
                        confirmed,
                        ..
                    } => {
                        // A marker never makes a branch the signature did
                        // not confirm `Exact`.
                        let (confidence, reason) =
                            evidence(exact_rule && confirmed, identity, &reasons);
                        match emitted.entry((source, target)) {
                            Entry::Occupied(slot) => {
                                let edge = &mut edges_out[*slot.get()];
                                if confidence > edge.confidence {
                                    edge.confidence = confidence;
                                    edge.reason = reason;
                                }
                            }
                            Entry::Vacant(slot) => {
                                slot.insert(edges_out.len());
                                edges_out.push(Edge {
                                    source,
                                    target,
                                    rel_type: RelType::Overrides,
                                    confidence,
                                    reason,
                                });
                            }
                        }
                        continue;
                    }
                    Stop::Ambiguous { ty, why } => {
                        let base = name_of(ty);
                        (
                            (source, 0, ty, 0),
                            BS_KIND_OVERRIDE_AMBIGUOUS,
                            match why {
                                Ambiguity::Several(n) => format!(
                                    "{n} methods named `{name}` in base `{base}`; no Overrides edge"
                                ),
                                Ambiguity::NotComparable => format!(
                                    "`{name}` in base `{base}`: signature not comparable; no Overrides edge"
                                ),
                                Ambiguity::NotVirtual => format!(
                                    "`{name}` in base `{base}` is not declared virtual; no Overrides edge"
                                ),
                                Ambiguity::Nullable => format!(
                                    "`{name}` in base `{base}` differs only in nullable annotations, which may name another value type; no Overrides edge"
                                ),
                                Ambiguity::Arity { base: theirs, own } => format!(
                                    "`{name}` in base `{base}` takes {theirs} parameters, this `{name}` takes {own}; no Overrides edge"
                                ),
                            },
                        )
                    }
                    Stop::Gap { owner, index, base } => {
                        let (kind, gap) = gap_finding(symbol_table, owner, base);
                        (
                            (source, 1, owner, index as u32),
                            kind,
                            format!(
                                "base `{}` of `{}` {gap}; no Overrides edge for `{name}`",
                                base.spelling.trim(),
                                name_of(owner)
                            ),
                        )
                    }
                    Stop::Incomplete(Incomplete { at, why }) => (
                        (source, 2, at, 0),
                        BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE,
                        match why {
                            Why::Cycle => format!(
                                "heritage cycle at `{}`; the lookup of `{name}` stopped",
                                name_of(at)
                            ),
                            Why::Depth => format!(
                                "heritage deeper than {MAX_OVERRIDE_DEPTH} levels at `{}`; the lookup of `{name}` stopped",
                                name_of(at)
                            ),
                            Why::Inconsistent => format!(
                                "no consistent method resolution order for `{}`; the lookup of `{name}` stopped",
                                name_of(at)
                            ),
                        },
                    ),
                };
                if reported.insert(key) {
                    blind_spots.push(BlindSpotRecord {
                        kind: string_pool.add(kind),
                        file_path: string_pool.add(&path),
                        start_row: node.span.0,
                        start_col: node.span.1,
                        end_row: node.span.2,
                        end_col: node.span.3,
                        hint: string_pool.add(&hint),
                        is_test,
                    });
                }
            }
        }
    }
    emitted.len()
}

/// The BlindSpot kind of a declared base that binds no one project type, and
/// the failure the hint states, with the candidate count.
fn gap_finding(table: &SymbolTable, owner: u32, base: &DeclaredBase) -> (&'static str, String) {
    let candidates = base.candidates.len();
    if candidates >= 2 {
        return (
            BS_KIND_OVERRIDE_BASE_AMBIGUOUS,
            format!("resolves to {candidates} candidate types"),
        );
    }
    let meta = table
        .file_of(owner)
        .map_or_else(FileMeta::default, FileMeta::from_path);
    let same_named = table
        .global_candidates(simple_type_name(&base.spelling), ResolveTarget::Type, meta)
        .len();
    match (candidates, same_named) {
        (1, n) => (
            BS_KIND_OVERRIDE_BASE_AMBIGUOUS,
            format!(
                "resolves to {} candidate types by path proximity only",
                n.max(1)
            ),
        ),
        (_, n) if n >= 2 => (
            BS_KIND_OVERRIDE_BASE_AMBIGUOUS,
            format!("resolves to none of {n} candidate types"),
        ),
        (_, n) => (
            BS_KIND_OVERRIDE_BASE_UNRESOLVED,
            format!("resolves to no project type ({n} same-named candidates)"),
        ),
    }
}

/// Go: each embedded type that declares the name. No search beyond it.
fn embedded_namesakes<'a>(
    table: &'a SymbolTable,
    methods: &MethodIndex<'_>,
    matcher: Matcher<'_>,
    owner: u32,
    name: &str,
) -> Vec<Stop<'a>> {
    table
        .declared_bases(owner)
        .iter()
        .filter_map(bind)
        .filter_map(|(ty, identity)| matcher.stop(ty, methods.get(&(ty, name))?, identity, false))
        .collect()
}

/// Rust: the trait the method's `impl` block names (its `heritage`), never a
/// supertrait of it.
fn named_trait<'a>(
    table: &'a SymbolTable,
    methods: &MethodIndex<'_>,
    matcher: Matcher<'_>,
    owner: u32,
    name: &str,
    heritage: &[String],
) -> Vec<Stop<'a>> {
    table
        .declared_bases(owner)
        .iter()
        .enumerate()
        .filter(|(_, base)| {
            let spelling = base.spelling.trim();
            !spelling.is_empty() && heritage.iter().any(|h| h.trim() == spelling)
        })
        .filter_map(|(index, base)| match bind(base) {
            None => Some(Stop::Gap { owner, index, base }),
            Some((ty, identity)) => matcher.stop(ty, methods.get(&(ty, name))?, identity, false),
        })
        .collect()
}

/// The lookup of one name through [`Chain`] orders, depth first.
struct Search<'a> {
    table: &'a SymbolTable,
    methods: &'a MethodIndex<'a>,
    matcher: Matcher<'a>,
    chain: Chain,
    name: &'a str,
    stops: Vec<Stop<'a>>,
    /// The highest identity each searched type, reached through a generic
    /// base or not, was searched with, and whether its search settled the
    /// chain.
    seen: FxHashMap<(u32, bool), (f32, bool)>,
    /// The types from the method's own type to the current one.
    path: Vec<u32>,
}

impl<'a> Search<'a> {
    /// Searches the bases of `ty`. True when a chain base settled: found
    /// the name, or met a gap. `generic`: a base on the path so far was
    /// spelled with type arguments.
    fn bases(&mut self, ty: u32, identity: f32, generic: bool) -> bool {
        let table = self.table;
        let bases = table.declared_bases(ty);
        let (chain, branches) = lookup_order(self.chain, table, bases);
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
        base: &'a DeclaredBase,
        identity: f32,
        generic: bool,
    ) -> bool {
        match bind(base) {
            None => {
                self.stops.push(Stop::Gap { owner, index, base });
                true
            }
            // The weakest hop of the path is the identity of every edge
            // past it.
            Some((ty, confidence)) => self.visit(
                ty,
                identity.min(confidence),
                generic || base.spelling.contains('<'),
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
        let stop = self
            .methods
            .get(&(ty, self.name))
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
fn lookup_order(
    chain: Chain,
    table: &SymbolTable,
    bases: &[DeclaredBase],
) -> (Vec<usize>, Vec<usize>) {
    let declared: Vec<usize> = (0..bases.len())
        .filter(|&i| !bases[i].spelling.trim().is_empty())
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
                bind(&bases[i]).is_some_and(|(id, _)| table.node_kind(id) == NodeKind::Trait)
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

/// One entry of a Python method resolution order: a type, or a declared base
/// that binds no one project type, kept as an opaque class with no bases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MroEntry {
    Type(u32),
    Gap { owner: u32, index: usize },
}

type Mro = Result<Vec<MroEntry>, Incomplete>;

/// Python: the first entry after the type in its C3 order that declares an
/// applicable method of the name, or is a gap.
fn mro_lookup<'a>(
    table: &'a SymbolTable,
    methods: &MethodIndex<'_>,
    matcher: Matcher<'_>,
    owner: u32,
    name: &str,
    mros: &mut FxHashMap<u32, Mro>,
) -> Vec<Stop<'a>> {
    let order = match mro(table, owner, mros, &mut Vec::new()) {
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
                    base: &table.declared_bases(declaring)[index],
                }]
            }
            MroEntry::Type(ty) => {
                let Some(decls) = methods.get(&(ty, name)) else {
                    continue;
                };
                let identity = widest_identity(table, owner, ty, &mut FxHashMap::default());
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
fn mro(table: &SymbolTable, ty: u32, memo: &mut FxHashMap<u32, Mro>, path: &mut Vec<u32>) -> Mro {
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
    let order = mro_of_bases(table, ty, memo, path);
    path.pop();
    memo.insert(ty, order.clone());
    order
}

fn mro_of_bases(
    table: &SymbolTable,
    ty: u32,
    memo: &mut FxHashMap<u32, Mro>,
    path: &mut Vec<u32>,
) -> Mro {
    let mut sequences: Vec<Vec<MroEntry>> = Vec::new();
    let mut heads: Vec<MroEntry> = Vec::new();
    for (index, base) in table.declared_bases(ty).iter().enumerate() {
        if base.spelling.trim().is_empty() {
            continue;
        }
        let head = match bind(base) {
            Some((id, _)) => {
                sequences.push(mro(table, id, memo, path)?);
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
fn widest_identity(table: &SymbolTable, from: u32, to: u32, memo: &mut FxHashMap<u32, f32>) -> f32 {
    if from == to {
        return 1.0;
    }
    if let Some(&known) = memo.get(&from) {
        return known;
    }
    memo.insert(from, 0.0);
    let mut best = 0.0f32;
    for base in table.declared_bases(from) {
        if let Some((id, confidence)) = bind(base) {
            best = best.max(confidence.min(widest_identity(table, id, to, memo)));
        }
    }
    memo.insert(from, best);
    best
}

/// Per raw node, the raw index of its owner type (module doc), or `None`.
///
/// One sweep over the types and callables in source order keeps the open
/// containers on a stack, so a file costs O(n log n), not one scan of every
/// container per callable. Spans of one tree nest or are disjoint; between
/// two identical spans the first stays the container.
fn owner_types(nodes: &[RawNode]) -> Vec<Option<usize>> {
    let is_container = |i: usize| holds_heritage(nodes[i].kind) || nodes[i].kind.is_callable();
    let mut order: Vec<usize> = (0..nodes.len()).filter(|&i| is_container(i)).collect();
    order.sort_by_key(|&i| {
        let (r1, c1, r2, c2) = nodes[i].span;
        ((r1, c1), Reverse((r2, c2)), i)
    });
    let mut innermost: Vec<Option<usize>> = vec![None; nodes.len()];
    let mut stack: Vec<usize> = Vec::new();
    for &i in &order {
        let span = nodes[i].span;
        while let Some(&top) = stack.last() {
            if span_contains(nodes[top].span, span) {
                break;
            }
            stack.pop();
        }
        match stack.last() {
            Some(&top) if nodes[top].span == span => innermost[i] = innermost[top],
            top => {
                innermost[i] = top.copied();
                stack.push(i);
            }
        }
    }
    let mut types_by_name: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
    for (i, n) in nodes.iter().enumerate() {
        if holds_heritage(n.kind) {
            types_by_name.entry(n.name.as_str()).or_default().push(i);
        }
    }
    (0..nodes.len())
        .map(|i| {
            if !nodes[i].kind.is_callable() {
                return None;
            }
            match innermost[i] {
                // A function inside a callable is a local, not a member.
                Some(j) => holds_heritage(nodes[j].kind).then_some(j),
                None => {
                    let owner = nodes[i].owner_class.as_deref()?.trim();
                    match types_by_name.get(owner)?.as_slice() {
                        [only] => Some(*only),
                        _ => None,
                    }
                }
            }
        })
        .collect()
}

/// Per raw node, the function meta whose span is the node's, as Pass 1.8
/// pairs them; `None` for a non-callable or an unpaired node.
fn metas_by_node(lg: &LocalGraph) -> Vec<Option<&RawFunctionMeta>> {
    let mut by_span: Vec<&RawFunctionMeta> = lg.raw_function_metas.iter().collect();
    by_span.sort_by_key(|m| m.span);
    lg.nodes
        .iter()
        .map(|n| {
            n.kind.is_callable().then_some(())?;
            let i = by_span.binary_search_by_key(&n.span, |m| m.span).ok()?;
            Some(by_span[i])
        })
        .collect()
}

/// The declarations of each method name per owner type. The parts of a C#
/// `partial` type are one owner, keyed by its first part
/// (`SymbolTable::partial_type`); each declaration keeps its own slot, so
/// an edge targets the method in the part that declares it.
fn build_method_index<'a>(
    local_graphs: &'a [LocalGraph],
    owners: &[Vec<Option<usize>>],
    metas: &[Vec<Option<&'a RawFunctionMeta>>],
    raw_map: &RawIndexMap,
    symbol_table: &SymbolTable,
) -> MethodIndex<'a> {
    let mut out: MethodIndex<'a> = FxHashMap::default();
    for (file_idx, lg) in local_graphs.iter().enumerate() {
        let file = FileOrd(file_idx as u32);
        for (raw_idx, node) in lg.nodes.iter().enumerate() {
            let name = node.name.trim();
            if !matches!(node.kind, NodeKind::Method | NodeKind::Function) || name.is_empty() {
                continue;
            }
            let Some(owner) = owners[file_idx][raw_idx] else {
                continue;
            };
            let (Some(owner_slot), Some(slot)) = (
                raw_map.node(file, RawIdx(owner as u32)),
                raw_map.node(file, RawIdx(raw_idx as u32)),
            ) else {
                continue;
            };
            // A tombstoned twin maps to the survivor: one method, not two.
            let decls = out
                .entry((symbol_table.partial_type(owner_slot), name))
                .or_default();
            if !decls.iter().any(|d| d.slot == slot) {
                decls.push(Decl {
                    slot,
                    meta: metas[file_idx][raw_idx],
                });
            }
        }
    }
    out
}

/// The name of the raw node at `slot`.
fn slot_name<'a>(
    local_graphs: &'a [LocalGraph],
    raw_map: &RawIndexMap,
    slot: u32,
) -> Option<&'a str> {
    let (file, raw) = raw_map.locate(slot)?;
    local_graphs
        .get(file.0 as usize)?
        .nodes
        .get(raw.index())
        .map(|n| n.name.as_str())
}

/// The last path segment of a heritage spelling, without generic or call
/// arguments: `fmt::Display` → `Display`, `Base<T>` → `Base`,
/// `App\Base` → `Base`, `Base()` → `Base`.
fn simple_type_name(heritage: &str) -> &str {
    let head = heritage
        .split(['<', '[', '('])
        .next()
        .unwrap_or(heritage)
        .trim();
    head.rsplit([':', '.', '\\']).next().unwrap_or(head).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_type_name_path_and_generics_returns_last_segment() {
        assert_eq!(simple_type_name("fmt::Display"), "Display");
        assert_eq!(simple_type_name("Base<T>"), "Base");
        assert_eq!(simple_type_name("a.b.Base"), "Base");
        assert_eq!(simple_type_name(" Base "), "Base");
        assert_eq!(simple_type_name("App\\Base"), "Base");
        assert_eq!(simple_type_name("Base()"), "Base");
        assert_eq!(simple_type_name("Generic[T]"), "Generic");
        assert_eq!(simple_type_name("   "), "");
    }

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

    fn reasons() -> (StringPool, Reasons) {
        let mut pool = StringPool::default();
        let reasons = Reasons {
            exact: pool.add(EXACT_REASON),
            name: pool.add(NAME_MATCH_REASON),
            base_by_name: pool.add(BASE_BY_NAME_REASON),
        };
        (pool, reasons)
    }

    /// Stands for: the confidence classes a machine reader tells apart by
    /// band and `reason` (C4).
    #[test]
    fn test_evidence_identity_bands_return_class_and_reason() {
        let (_pool, r) = reasons();
        assert_eq!(evidence(true, 1.0, &r), (1.0, r.exact), "same-file marked");
        assert_eq!(
            evidence(true, 0.95, &r),
            (0.95, r.exact),
            "an imported base carries its confidence, a marker does not raise it"
        );
        assert_eq!(evidence(false, 1.0, &r), (NAME_MATCH_CONFIDENCE, r.name));
        assert_eq!(
            evidence(true, HEURISTIC_CEILING, &r),
            (NAME_MATCH_CONFIDENCE, r.base_by_name),
            "a bare-name base is never Exact, even when marked"
        );
        assert_eq!(
            evidence(false, HEURISTIC_CEILING, &r),
            (NAME_MATCH_CONFIDENCE, r.base_by_name)
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
        let base = |candidates: &[(u32, f32)]| DeclaredBase {
            spelling: "Base".into(),
            candidates: candidates.into(),
        };
        assert_eq!(bind(&base(&[(1, 1.0)])), Some((1, 1.0)));
        assert_eq!(bind(&base(&[(1, 0.7)])), Some((1, 0.7)));
        assert_eq!(bind(&base(&[(1, NEAREST_COPY_CONFIDENCE)])), None);
        assert_eq!(bind(&base(&[(1, 1.0), (2, 1.0)])), None);
        assert_eq!(bind(&base(&[])), None);
    }
}
