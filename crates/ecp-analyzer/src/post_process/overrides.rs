//! `Overrides` edge emission: a method → the same-name method of a declared
//! base of its type.
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
//! (`SymbolTable::supertypes`), so another type that only shares a base's
//! name is never reached. A base outside the graph (unresolved, or in a file
//! that failed to parse) gives no edge.
//!
//! # Evidence per language
//!
//! | Rule | Languages | Candidate | Edge |
//! |---|---|---|---|
//! | `Marked` | Java, Kotlin, C#, C++, others | a method the parser marked (`@Override`, `override`) | 1.0, `Exact` |
//! | `Inferred` | TypeScript, JavaScript, Python, PHP, Ruby, Swift, Dart | a marked method, else any method of a type with bases | marked: 1.0; unmarked: [`NAME_MATCH_CONFIDENCE`], `Heuristic` |
//! | `TraitImpl` | Rust | a method of an `impl Trait for T` block, which carries the trait as heritage | 1.0, `Exact`, only into that trait |
//! | `NoConcept` | Go, C | none | no edge; Go records a [`BS_KIND_NO_OVERRIDE_CONCEPT`] BlindSpot where a method shadows one of an embedded type |
//!
//! The parsers of the `Inferred` languages capture no mandatory marker, so
//! an unmarked method there pairs with its base by name alone, and the edge
//! stays below `HEURISTIC_CEILING` with its own `reason` (C4 evidence
//! classes).
//!
//! # Immediate supertype only
//!
//! For `C extends B extends A` where all three define `foo()`, the edges are
//! `C.foo → B.foo` and `B.foo → A.foo`. C overrides B's contract; a 2-hop
//! query reads the chain, and a skip-level edge would add no fact.
//!
//! # Ambiguity
//!
//! A base that declares the name more than once (overloads) gives no edge
//! and a [`BS_KIND_OVERRIDE_AMBIGUOUS`] BlindSpot with the count: the pass
//! has no parameter types to pick one.

use crate::resolution::index::{Language, SymbolTable};
use ecp_core::analyzer::heritage::holds_heritage;
use ecp_core::analyzer::types::{span_contains, LocalGraph, RawNode};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::graph::{BlindSpotRecord, Edge, File, FileCategory, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cmp::Reverse;

/// BlindSpot kind: a Go method shadows a same-name method of a type its
/// receiver embeds. Go has no override, so no `Overrides` edge exists.
pub const BS_KIND_NO_OVERRIDE_CONCEPT: &str = "no-override-concept";

/// BlindSpot kind: a declared base declares the method name more than once.
pub const BS_KIND_OVERRIDE_AMBIGUOUS: &str = "override-ambiguous";

/// The confidence of an edge paired by name alone: `Heuristic`, below the
/// 0.7 ceiling.
pub const NAME_MATCH_CONFIDENCE: f32 = 0.6;

const EXACT_REASON: &str = "post_process:overrides";
const NAME_MATCH_REASON: &str = "post_process:overrides:name-in-base";

/// Sentinel written by per-language parsers when the `override` keyword /
/// annotation is present. Java writes `@Override` (the literal annotation
/// text); Kotlin / C# / C++ write this sentinel via the `@override_marker`
/// query capture.
const OVERRIDE_SENTINEL: &str = "__override__";

/// `(type slot, method name)` → the slots of the methods so named that the
/// type declares.
type MethodIndex = FxHashMap<(u32, String), Vec<u32>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverrideRule {
    Marked,
    Inferred,
    TraitImpl,
    NoConcept,
}

fn rule_for(path: &str) -> OverrideRule {
    match Language::from_path(path) {
        Language::TypeScript
        | Language::JavaScript
        | Language::Python
        | Language::Php
        | Language::Ruby
        | Language::Swift
        | Language::Dart => OverrideRule::Inferred,
        Language::Rust => OverrideRule::TraitImpl,
        Language::Go | Language::C => OverrideRule::NoConcept,
        _ => OverrideRule::Marked,
    }
}

/// Java `@Override` (also fully qualified), Dart `@override`, Python
/// `@override` / `@typing.override`, and the Kotlin / C# / C++ sentinel.
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

/// Emit `Overrides` edges for all languages, and the BlindSpots of the
/// module doc.
///
/// Requires the supertypes of Pass 1.9 in `symbol_table`. Returns the number
/// of edges appended.
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
    let exact_reason = string_pool.add(EXACT_REASON);
    let name_reason = string_pool.add(NAME_MATCH_REASON);

    let mut emitted: FxHashSet<(u32, u32)> = FxHashSet::default();
    let mut reported: FxHashSet<(u32, u32)> = FxHashSet::default();
    for (file_idx, lg) in local_graphs.iter().enumerate() {
        let file = FileOrd(file_idx as u32);
        let path = lg.file_path.to_string_lossy().replace('\\', "/");
        let rule = rule_for(&path);
        let is_test = files
            .get(file_idx)
            .is_some_and(|f| matches!(f.category, FileCategory::Test));
        for (raw_idx, node) in lg.nodes.iter().enumerate() {
            let name = node.name.trim();
            if !node.kind.is_callable() || name.is_empty() {
                continue;
            }
            let marked = is_marked(node);
            let exact = match rule {
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
            let Some(supertypes) = symbol_table.supertypes(owner_slot) else {
                continue;
            };
            for &base in supertypes.bases.iter() {
                // A Rust impl method implements the trait its block names,
                // not every trait its type implements.
                if rule == OverrideRule::TraitImpl
                    && !node.heritage.iter().any(|h| {
                        Some(simple_type_name(h)) == slot_name(local_graphs, raw_map, base)
                    })
                {
                    continue;
                }
                let Some(targets) = methods.get(&(base, name.to_string())) else {
                    continue;
                };
                let blind_spot = match (rule, targets.as_slice()) {
                    (_, [target]) if *target == source => continue,
                    (OverrideRule::NoConcept, [_]) => Some((
                        BS_KIND_NO_OVERRIDE_CONCEPT,
                        format!(
                            "`{name}` shadows the `{name}` of the embedded `{}`; the language has no method override, so no Overrides edge",
                            slot_name(local_graphs, raw_map, base).unwrap_or("")
                        ),
                    )),
                    (_, [target]) => {
                        if emitted.insert((source, *target)) {
                            edges_out.push(Edge {
                                source,
                                target: *target,
                                rel_type: RelType::Overrides,
                                confidence: if exact { 1.0 } else { NAME_MATCH_CONFIDENCE },
                                reason: if exact { exact_reason } else { name_reason },
                            });
                        }
                        None
                    }
                    (_, several) => Some((
                        BS_KIND_OVERRIDE_AMBIGUOUS,
                        format!(
                            "{} methods named `{name}` in base `{}`; no Overrides edge",
                            several.len(),
                            slot_name(local_graphs, raw_map, base).unwrap_or("")
                        ),
                    )),
                };
                if let Some((kind, hint)) = blind_spot {
                    if reported.insert((source, base)) {
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
    }
    emitted.len()
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

fn build_method_index(
    local_graphs: &[LocalGraph],
    owners: &[Vec<Option<usize>>],
    raw_map: &RawIndexMap,
) -> MethodIndex {
    let mut out: MethodIndex = FxHashMap::default();
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
            let slots = out.entry((owner_slot, name.to_string())).or_default();
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

/// The last path segment of a heritage spelling, without generic arguments:
/// `fmt::Display` → `Display`, `Base<T>` → `Base`.
fn simple_type_name(heritage: &str) -> &str {
    let head = heritage.split('<').next().unwrap_or(heritage).trim();
    head.rsplit([':', '.']).next().unwrap_or(head).trim()
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
}
