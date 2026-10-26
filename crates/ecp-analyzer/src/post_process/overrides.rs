//! `Overrides` edge emission over the whole index, and its BlindSpots.
//!
//! The derivation (the rules per language, the signature match, the lookup
//! orders, the C3 merge, the evidence and reason) lives in
//! [`ecp_core::analyzer::overrides`], whose module doc states it; the
//! overlay's replay for a dirty file calls the same functions. This pass
//! answers its facts from the `SymbolTable` (the declared bases of Pass 1.9,
//! the C# `partial` grouping) and every parse (the methods per owner type,
//! each with the function meta paired by span), and writes each gap the
//! lookup meets as a BlindSpot at the method, deduplicated per method and
//! base.

use crate::resolution::index::{DeclaredBase, FileMeta, ResolveTarget, SymbolTable};
use ecp_core::analyzer::overrides::{
    method_lookup, owner_types, rules_for, Ambiguity, Decl, Incomplete, MethodIndex, MroCache,
    OverrideBase, OverrideEdges, OverrideFacts, OverrideMethod, Stop, Why,
    OVERRIDES_BASE_BY_NAME_REASON, OVERRIDES_NAME_MATCH_REASON, OVERRIDES_REASON,
};
pub use ecp_core::analyzer::overrides::{
    BS_KINDS, BS_KIND_NO_OVERRIDE_CONCEPT, BS_KIND_OVERRIDE_AMBIGUOUS,
    BS_KIND_OVERRIDE_BASE_AMBIGUOUS, BS_KIND_OVERRIDE_BASE_UNRESOLVED,
    BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE, MAX_OVERRIDE_DEPTH, NAME_MATCH_CONFIDENCE,
};
use ecp_core::analyzer::types::{owner_key, LocalGraph, RawFunctionMeta};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::graph::{BlindSpotRecord, Edge, File, FileCategory, NodeKind, RelType};
use ecp_core::pool::{StrRef, StringPool};
use rustc_hash::{FxHashMap, FxHashSet};

impl OverrideBase for DeclaredBase {
    fn spelling(&self) -> &str {
        &self.spelling
    }

    fn candidates(&self) -> &[(u32, f32)] {
        &self.candidates
    }
}

/// The index's answers to the derivation: the declared bases and the
/// `partial` grouping of the `SymbolTable`, and the methods of every parse.
struct TableFacts<'a> {
    table: &'a SymbolTable,
    methods: MethodIndex<'a>,
}

impl OverrideFacts for TableFacts<'_> {
    type Base = DeclaredBase;

    fn declared_bases(&self, ty: u32) -> &[DeclaredBase] {
        self.table.declared_bases(ty)
    }

    fn partial_type(&self, ty: u32) -> u32 {
        self.table.partial_type(ty)
    }

    fn node_kind(&self, id: u32) -> NodeKind {
        self.table.node_kind(id)
    }

    fn methods<'s>(&'s self, ty: u32, name: &'s str) -> Option<&'s [Decl<'s>]> {
        self.methods.get(&(ty, name)).map(Vec::as_slice)
    }
}

/// The interned `reason` of each edge class.
struct Reasons {
    exact: StrRef,
    name: StrRef,
    base_by_name: StrRef,
}

impl Reasons {
    fn of(&self, reason: &str) -> StrRef {
        match reason {
            OVERRIDES_REASON => self.exact,
            OVERRIDES_NAME_MATCH_REASON => self.name,
            _ => self.base_by_name,
        }
    }
}

/// Emit `Overrides` edges for all languages, and the BlindSpots of the
/// derivation's module doc.
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
    let facts = TableFacts {
        table: symbol_table,
        methods: build_method_index(local_graphs, &owners, &metas, raw_map, symbol_table),
    };
    let reasons = Reasons {
        exact: string_pool.add(OVERRIDES_REASON),
        name: string_pool.add(OVERRIDES_NAME_MATCH_REASON),
        base_by_name: string_pool.add(OVERRIDES_BASE_BY_NAME_REASON),
    };
    let name_of = |slot: u32| slot_name(local_graphs, raw_map, slot).unwrap_or("");

    let mut emitted = OverrideEdges::default();
    let mut reported: FxHashSet<(u32, u8, u32, u32)> = FxHashSet::default();
    let mut mros = MroCache::default();
    // The gap finding of a declared base depends on the base alone, and
    // every method of the type that reaches it would count the same
    // same-named candidates again.
    let mut gaps: FxHashMap<(u32, usize), (&'static str, String)> = FxHashMap::default();
    for (file_idx, lg) in local_graphs.iter().enumerate() {
        let file = FileOrd(file_idx as u32);
        let path = lg.file_path.to_string_lossy().replace('\\', "/");
        let Some(rules) = rules_for(symbol_table.file_language(&path)) else {
            continue;
        };
        let is_test = files
            .get(file_idx)
            .is_some_and(|f| matches!(f.category, FileCategory::Test));
        for (raw_idx, node) in lg.nodes.iter().enumerate() {
            let Some(owner) = owners[file_idx][raw_idx] else {
                continue;
            };
            let (Some(owner_slot), Some(source)) = (
                raw_map.node(file, RawIdx(owner as u32)),
                raw_map.node(file, RawIdx(raw_idx as u32)),
            ) else {
                continue;
            };
            let method = OverrideMethod {
                name: &node.name,
                kind: node.kind,
                decorators: &node.decorators,
                heritage: &node.heritage,
                meta: metas[file_idx][raw_idx],
            };
            let Some(lookup) = method_lookup(&facts, rules, &method, owner_slot, &mut mros) else {
                continue;
            };
            let name = node.name.trim();
            for &stop in &lookup.stops {
                if let Some(edge) = lookup.edge(source, stop) {
                    emitted.add(edge);
                    continue;
                }
                let (key, kind, hint) = match stop {
                    Stop::Found { target, .. } if target == source => continue,
                    // `edge` gives every other found declaration its edge,
                    // except in a language with no override (Go).
                    Stop::Found { ty, .. } => (
                        (source, 3, ty, 0),
                        BS_KIND_NO_OVERRIDE_CONCEPT,
                        format!(
                            "`{name}` shadows the `{name}` of the embedded `{}`; the language has no method override, so no Overrides edge",
                            name_of(ty)
                        ),
                    ),
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
                    Stop::Gap { owner, index } => {
                        let base = &symbol_table.declared_bases(owner)[index];
                        let (kind, gap) = gaps
                            .entry((owner, index))
                            .or_insert_with(|| gap_finding(symbol_table, owner, base));
                        (
                            (source, 1, owner, index as u32),
                            *kind,
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
    let edges = emitted.into_edges();
    let count = edges.len();
    edges_out.extend(edges.into_iter().map(|edge| Edge {
        source: edge.source,
        target: edge.target,
        rel_type: RelType::Overrides,
        confidence: edge.confidence,
        reason: reasons.of(edge.reason),
    }));
    count
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
        .map_or_else(FileMeta::default, |file| table.file_meta(file));
    let same_named = table
        .global_candidates(
            owner_key(&base.spelling).unwrap_or(""),
            ResolveTarget::Type,
            meta,
        )
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
/// `partial` type are one owner, keyed by its representative part
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands for: a gap counts the same-named candidates of the type the
    /// base spelling names, its last segment. `Outer<int>.Inner` names
    /// `Inner`; the two project types named `Outer` are no candidates for
    /// it, so the gap is unresolved. On HEAD the count cut the spelling at
    /// `<` and read `Outer`: "resolves to none of 2 candidate types",
    /// `override-base-ambiguous`.
    #[test]
    fn test_gap_finding_generic_outer_segment_counts_last_segment_unresolved() {
        let mut table = SymbolTable::new();
        table.register_node("src/a/Outer.cs", "Outer", 0, NodeKind::Class);
        table.register_node("src/b/Outer.cs", "Outer", 1, NodeKind::Class);
        table.register_node("src/Child.cs", "Child", 2, NodeKind::Class);
        let base = |spelling: &str| DeclaredBase {
            spelling: spelling.into(),
            candidates: Box::default(),
        };
        assert_eq!(
            gap_finding(&table, 2, &base("Outer<int>.Inner")),
            (
                BS_KIND_OVERRIDE_BASE_UNRESOLVED,
                "resolves to no project type (0 same-named candidates)".to_string()
            )
        );
        assert_eq!(
            gap_finding(&table, 2, &base("Outer<int>")),
            (
                BS_KIND_OVERRIDE_BASE_AMBIGUOUS,
                "resolves to none of 2 candidate types".to_string()
            ),
            "control: the table holds both `Outer` types"
        );
        assert_eq!(
            gap_finding(&table, 2, &base("   ")),
            (
                BS_KIND_OVERRIDE_BASE_UNRESOLVED,
                "resolves to no project type (0 same-named candidates)".to_string()
            )
        );
    }
}
