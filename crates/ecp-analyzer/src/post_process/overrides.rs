//! `Overrides` edge emission: a method → the nearest same-name declaration
//! along the declared bases of its type.
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
//! declares the name: for `C → B → A`, `C.m → A.m` when B lacks `m`, and
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
//! # BlindSpots
//!
//! The lookup never skips a gap to reach a farther ancestor. A gap ends its
//! path with a BlindSpot at the method, deduplicated per method and base:
//!
//! | Kind | Gap |
//! |---|---|
//! | [`BS_KIND_OVERRIDE_BASE_UNRESOLVED`] | a declared base resolved to no project type (external, or a resolution failure) |
//! | [`BS_KIND_OVERRIDE_BASE_AMBIGUOUS`] | a declared base names several project types, or one picked by path proximity alone |
//! | [`BS_KIND_OVERRIDE_AMBIGUOUS`] | the nearest base declares the name more than once: the pass has no parameter types to pick one |
//! | [`BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE`] | a heritage cycle, more than [`MAX_OVERRIDE_DEPTH`] levels, or no consistent Python MRO |

use crate::resolution::index::{DeclaredBase, FileMeta, Language, ResolveTarget, SymbolTable};
use ecp_core::analyzer::heritage::holds_heritage;
use ecp_core::analyzer::types::{span_contains, LocalGraph, RawNode, NEAREST_COPY_CONFIDENCE};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap, HEURISTIC_CEILING};
use ecp_core::graph::{BlindSpotRecord, Edge, File, FileCategory, NodeKind, RelType};
use ecp_core::pool::{StrRef, StringPool};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cmp::Reverse;
use std::collections::hash_map::Entry;

/// BlindSpot kind: a Go method shadows a same-name method of a type its
/// receiver embeds. Go has no override, so no `Overrides` edge exists.
pub const BS_KIND_NO_OVERRIDE_CONCEPT: &str = "no-override-concept";

/// BlindSpot kind: the nearest base on the lookup path declares the method
/// name more than once.
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

/// `(type slot, method name)` → the slots of the methods so named that the
/// type declares.
type MethodIndex<'a> = FxHashMap<(u32, &'a str), Vec<u32>>;

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

/// The rule and lookup of the file at `path`; `None` for C, which has no
/// override.
fn rules_for(path: &str) -> Option<(OverrideRule, Lookup)> {
    use OverrideRule::*;
    Some(match Language::from_path(path) {
        Language::TypeScript | Language::JavaScript | Language::Php | Language::Swift => {
            (Inferred, Lookup::Chain(Chain::PerBase))
        }
        Language::Python => (Inferred, Lookup::Mro),
        Language::Ruby => (Inferred, Lookup::Chain(Chain::Ancestors)),
        Language::Dart => (Inferred, Lookup::Chain(Chain::Mixins)),
        Language::Rust => (TraitImpl, Lookup::Chain(Chain::PerBase)),
        Language::Go => (NoConcept, Lookup::Chain(Chain::PerBase)),
        Language::C => return None,
        _ => (Marked, Lookup::Chain(Chain::PerBase)),
    })
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
    /// The declarations of the name in `ty`. `identity` is the weakest
    /// resolver confidence on the path to `ty`.
    Found {
        ty: u32,
        targets: &'a [u32],
        identity: f32,
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
    let methods = build_method_index(local_graphs, &owners, raw_map);
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
        let Some((rule, lookup)) = rules_for(&path) else {
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
                    embedded_namesakes(symbol_table, &methods, owner_slot, name)
                }
                (OverrideRule::TraitImpl, _) => {
                    named_trait(symbol_table, &methods, owner_slot, name, &node.heritage)
                }
                (_, Lookup::Mro) => mro_lookup(symbol_table, &methods, owner_slot, name, &mut mros),
                (_, Lookup::Chain(chain)) => {
                    let mut search = Search {
                        table: symbol_table,
                        methods: &methods,
                        chain,
                        name,
                        stops: Vec::new(),
                        seen: FxHashMap::default(),
                        path: vec![owner_slot],
                    };
                    search.bases(owner_slot, 1.0);
                    search.stops
                }
            };
            for stop in stops {
                let (key, kind, hint) = match stop {
                    Stop::Found {
                        targets: [only], ..
                    } if *only == source => continue,
                    Stop::Found { ty, .. } if rule == OverrideRule::NoConcept => (
                        (source, 3, ty, 0),
                        BS_KIND_NO_OVERRIDE_CONCEPT,
                        format!(
                            "`{name}` shadows the `{name}` of the embedded `{}`; the language has no method override, so no Overrides edge",
                            name_of(ty)
                        ),
                    ),
                    Stop::Found {
                        targets: [target],
                        identity,
                        ..
                    } => {
                        let (confidence, reason) = evidence(exact_rule, identity, &reasons);
                        match emitted.entry((source, *target)) {
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
                                    target: *target,
                                    rel_type: RelType::Overrides,
                                    confidence,
                                    reason,
                                });
                            }
                        }
                        continue;
                    }
                    Stop::Found { ty, targets, .. } => (
                        (source, 0, ty, 0),
                        BS_KIND_OVERRIDE_AMBIGUOUS,
                        format!(
                            "{} methods named `{name}` in base `{}`; no Overrides edge",
                            targets.len(),
                            name_of(ty)
                        ),
                    ),
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
    methods: &'a MethodIndex<'a>,
    owner: u32,
    name: &'a str,
) -> Vec<Stop<'a>> {
    table
        .declared_bases(owner)
        .iter()
        .filter_map(bind)
        .filter_map(|(ty, identity)| {
            methods.get(&(ty, name)).map(|targets| Stop::Found {
                ty,
                targets,
                identity,
            })
        })
        .collect()
}

/// Rust: the trait the method's `impl` block names (its `heritage`), never a
/// supertrait of it.
fn named_trait<'a>(
    table: &'a SymbolTable,
    methods: &'a MethodIndex<'a>,
    owner: u32,
    name: &'a str,
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
            Some((ty, identity)) => methods.get(&(ty, name)).map(|targets| Stop::Found {
                ty,
                targets,
                identity,
            }),
        })
        .collect()
}

/// The lookup of one name through [`Chain`] orders, depth first.
struct Search<'a> {
    table: &'a SymbolTable,
    methods: &'a MethodIndex<'a>,
    chain: Chain,
    name: &'a str,
    stops: Vec<Stop<'a>>,
    /// The highest identity each searched type was searched with, and
    /// whether its search settled the chain.
    seen: FxHashMap<u32, (f32, bool)>,
    /// The types from the method's own type to the current one.
    path: Vec<u32>,
}

impl<'a> Search<'a> {
    /// Searches the bases of `ty`. True when a chain base settled: found
    /// the name, or met a gap.
    fn bases(&mut self, ty: u32, identity: f32) -> bool {
        let table = self.table;
        let bases = table.declared_bases(ty);
        let (chain, branches) = lookup_order(self.chain, table, bases);
        let settled = chain
            .into_iter()
            .any(|index| self.base(ty, index, &bases[index], identity));
        for index in branches {
            self.base(ty, index, &bases[index], identity);
        }
        settled
    }

    fn base(&mut self, owner: u32, index: usize, base: &'a DeclaredBase, identity: f32) -> bool {
        match bind(base) {
            None => {
                self.stops.push(Stop::Gap { owner, index, base });
                true
            }
            Some((ty, confidence)) => self.visit(ty, identity.min(confidence)),
        }
    }

    fn visit(&mut self, ty: u32, identity: f32) -> bool {
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
        let methods = self.methods;
        if let Some(targets) = methods.get(&(ty, self.name)) {
            self.stops.push(Stop::Found {
                ty,
                targets,
                identity,
            });
            return true;
        }
        if let Some(&(searched, settled)) = self.seen.get(&ty) {
            if searched >= identity {
                return settled;
            }
        }
        self.path.push(ty);
        let settled = self.bases(ty, identity);
        self.path.pop();
        self.seen.insert(ty, (identity, settled));
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

/// Python: the first entry after the type in its C3 order that declares the
/// name or is a gap.
fn mro_lookup<'a>(
    table: &'a SymbolTable,
    methods: &'a MethodIndex<'a>,
    owner: u32,
    name: &'a str,
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
                if let Some(targets) = methods.get(&(ty, name)) {
                    let identity = widest_identity(table, owner, ty, &mut FxHashMap::default());
                    return vec![Stop::Found {
                        ty,
                        targets,
                        identity,
                    }];
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

fn build_method_index<'a>(
    local_graphs: &'a [LocalGraph],
    owners: &[Vec<Option<usize>>],
    raw_map: &RawIndexMap,
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
            let slots = out.entry((owner_slot, name)).or_default();
            if !slots.contains(&slot) {
                slots.push(slot);
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
