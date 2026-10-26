//! Per-file mode of the incremental overlay (architecture direction, "Beyond
//! calls: the incremental overlay"): the outputs a fresh build makes for one
//! file from that file's parse alone, replayed for each dirty file from its
//! fragment's records. Every endpoint comes from the file's own raw indexes
//! through a [`RawIndexMap`]; no name lookup leaves the file, except a SQL
//! table's, which the index makes by name over every node too.
//!
//! | Output | Index derivation | Replay |
//! |---|---|---|
//! | `File` node | `GraphBuilder::build`, one per `LocalGraph` | one virtual node per input whose path has no entry in `graph.files`; a path that has one keeps its base `File` nodes |
//! | scope `Defines` | `post_process::scope_defines` | [`containment::scope_defines`] |
//! | enum containment | `post_process::enum_variant_defines` | [`containment::enum_variant_defines`] |
//! | class membership: `HasMethod` / `HasProperty` | `post_process::class_membership` | [`membership::class_membership`], `HasProperty` only with the records |
//! | `Imports` | `post_process::imports_edges` | [`crate::analyzer::import_edges`], resolved by the view's resolution pass against [`dirty_file_nodes`]; the replay appends them |
//! | function meta | `pass1_8_function_metas` | the flags and return type the fragment paired by span |
//! | `PathLiteral` + `UsesPathLiteral` | `post_process::path_literal_nodes` | [`record_nodes::path_literal_node`] |
//! | `QueriesTable` | `post_process::sql_table_edges` | [`record_nodes::sql_table_refs`], the table by [`Tables`] |
//! | `TransactionScope` + `OpensTxScope` | `post_process::tx_scope_edges` | [`record_nodes::tx_scope_node`] |
//! | `SchemaField` + `HasProperty` | `post_process::schema_field_mirrors` | [`record_nodes::schema_field_node`] |
//! | `EventTopic` + `Publishes` / `Subscribes` | `post_process::event_topic_mirrors` | [`record_nodes::event_topic_nodes`] |
//!
//! The five record outputs replay only when the caller hands the view each
//! file's [`OverlayFileRecords`]; without them a dirty file keeps the base
//! nodes and edges of those kinds as indexed. A base record node of a
//! re-parsed path pairs with the replayed node of its uid (the k-th with the
//! k-th), so a heuristic mirror edge into it from another file survives as
//! indexed; an unpaired one is suppressed. A `SchemaField` never pairs, so
//! its `MirrorsField` edges drop with it. Its `HasProperty` shares a kind
//! with class membership, and the view masks a dirty type's base
//! `HasProperty` by kind: class membership replays `HasProperty` only when
//! the records replay too, and keeps it as indexed otherwise.
//!
//! Class membership reads one path's name tables ([`PathDecls`]), the ones
//! the index's `SymbolTable` holds for the path after Pass 1: a later
//! overload, a C# arity twin and a tombstone are in none, and an overload or
//! a twin binds its own slot.
//!
//! [`OverlayView::build_with_records`](super::view::OverlayView::build_with_records)
//! calls [`replay`] once, after the virtual nodes and their tombstones are
//! final. Cost: O(dirty symbols + dirty records + base record nodes of the
//! dirty paths) hash operations, plus one O(symbols of the path) table for a
//! path that declares a type or an enum variant (a C# path also renumbers
//! its declarations once, O(symbols)), plus O(symbols x types of the file)
//! span checks for class membership, plus one name-index probe (O(log N)
//! and the namesakes) per distinct SQL table name. Nothing scans the base
//! graph: the build's one node scan collects the base record nodes.

use super::view::{
    reason_index, OverlayFileInput, OverlayFileRecords, ViewEdge, ViewFunctionMeta, ViewNode,
};
use crate::analyzer::containment::{
    self, Row, Scope, ENUM_VARIANT_DEFINES_REASON, SCOPE_DEFINES_REASON,
};
use crate::analyzer::heritage::HeritageNode;
use crate::analyzer::lexical::{file_identities, later_is_arity_twin, splits_by_arity};
use crate::analyzer::membership::{self, MemberLookup, MemberRow};
use crate::analyzer::record_nodes::{
    self, RecordNode, PATH_LITERAL_SINK_REASONS, SCHEMA_FIELD_HAS_PROPERTY_REASON, TX_SCOPE_REASON,
};
use crate::analyzer::types::owner_key;
use crate::contracts::identity::UidSlot;
use crate::contracts::raw_records::{FileOrd, GlobalEndpoint, RawEndpoint, RawIdx, RawIndexMap};
use crate::file_category::Language;
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// What the build hands the replay. Paths are `File.path`-normalized.
pub(super) struct ReplayInputs<'a> {
    pub graph: &'a ArchivedZeroCopyGraph,
    pub files: &'a [OverlayFileInput],
    /// Empty, or one per input of `files` in order: the record outputs then
    /// replay. Any other length replays none of them.
    pub records: &'a [OverlayFileRecords],
    pub base_len: u32,
    /// `(tombstone, survivor)` virtual indexes: each later declaration the
    /// index tombstones, with the first of its uid.
    pub tombstones: Vec<(u32, u32)>,
    /// The dirty paths `graph.files` holds.
    pub indexed: FxHashSet<&'a str>,
    /// Each dirty path's base `File` nodes, in graph order: one per
    /// `LocalGraph` of the path. The last is the one the index's
    /// `file_node_idx` keeps, so the one its `Defines` start at.
    pub base_files: FxHashMap<&'a str, Vec<u32>>,
    /// Each dirty path's `File` node by [`dirty_file_nodes`], which the
    /// `Imports` edges were resolved against before this replay numbered
    /// the new ones.
    pub dirty_files: &'a FxHashMap<&'a str, u32>,
    /// The dirty files' `Imports` edges, by the index's derivation
    /// ([`crate::analyzer::import_edges`]), sourced at `dirty_files`.
    pub imports: Vec<ViewEdge>,
    /// The base nodes of the dirty paths whose kind
    /// [`replays_record_kind`], ascending.
    pub base_records: Vec<u32>,
    /// The view's `dirty_base`: a base node of a dirty or removed path.
    pub dirty_base: &'a FxHashSet<u32>,
    /// Scoped virtual index → its lexical parent ([`super::view`]'s table).
    pub lexical_parents: &'a FxHashMap<u32, u32>,
    /// First virtual index → its later overloads, in source order, as the
    /// index's `SymbolTable::register_overload` numbers them.
    pub later_overloads: &'a FxHashMap<u32, Vec<u32>>,
}

/// The replay's rows beside the nodes and edges it appends.
#[derive(Debug, Default)]
pub(super) struct Replayed {
    /// The base `File` nodes of the re-parsed paths: the view masks the base
    /// `Defines` they source, and with [`Self::records_replayed`] their base
    /// record edges.
    pub defines_files: FxHashSet<u32>,
    /// Ascending by node.
    pub function_metas: Vec<ViewFunctionMeta>,
    /// Whether the record outputs replayed: the view then masks the base
    /// record edges of the dirty files.
    pub records_replayed: bool,
    /// `(base, virtual)`: a base record node and the replayed node of its uid.
    pub replaced: Vec<(u32, u32)>,
    /// The base record nodes no replayed node pairs with.
    pub suppressed: Vec<u32>,
}

/// Whether the record replay rebuilds a base node of `kind` for a re-parsed
/// path: the node kinds the five record outputs make.
pub(super) fn replays_record_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::PathLiteral
            | NodeKind::TransactionScope
            | NodeKind::SchemaField
            | NodeKind::EventTopic
    )
}

/// Each dirty path's `File` node, as [`replay`] numbers it: an `indexed`
/// path's last base `File` node (`base_files`), else the virtual node the
/// replay appends for the path's last input, the k-th input of an unindexed
/// path taking `first_new + k`. An indexed path with no base `File` node has
/// none.
pub(super) fn dirty_file_nodes<'a>(
    files: &'a [OverlayFileInput],
    indexed: &FxHashSet<&str>,
    base_files: &FxHashMap<&str, Vec<u32>>,
    first_new: u32,
) -> FxHashMap<&'a str, u32> {
    let mut table: FxHashMap<&'a str, u32> = FxHashMap::default();
    let mut next = first_new;
    for file in files {
        let path = file.rel_path.as_str();
        if indexed.contains(path) {
            if let Some(&last) = base_files.get(path).and_then(|all| all.last()) {
                table.insert(path, last);
            }
            continue;
        }
        table.insert(path, next);
        next += 1;
    }
    table
}

/// Append the per-file outputs of `inputs.files`: virtual `File` and record
/// nodes onto `nodes`, and `Defines` and record edges onto `edges` with
/// their adjacency. `nodes` holds every virtual symbol node, tombstones
/// applied. Returns nothing but the function metas when the inputs break
/// [`RawIndexMap`]'s invariants, which `file_identities` output never does:
/// no edge beats a wrong one.
pub(super) fn replay(
    inputs: ReplayInputs<'_>,
    nodes: &mut Vec<ViewNode>,
    edges: &mut Vec<ViewEdge>,
    out_adj: &mut FxHashMap<u32, Vec<u32>>,
    in_adj: &mut FxHashMap<u32, Vec<u32>>,
) -> Replayed {
    let ReplayInputs {
        graph,
        files,
        records,
        base_len,
        tombstones,
        indexed,
        base_files,
        dirty_files,
        imports,
        base_records,
        dirty_base,
        lexical_parents,
        later_overloads,
    } = inputs;
    let mut replayed = Replayed {
        function_metas: function_metas(files, base_len),
        ..Replayed::default()
    };
    let Some(raw_map) = RawIndexMap::new(base_len, files.iter().map(|f| f.symbols.len()))
        .and_then(|map| map.with_merged(tombstones))
        .ok()
    else {
        return replayed;
    };

    // One `File` node per input of a path the base never indexed, as the
    // index makes one per `LocalGraph`; the last one of a path is the one
    // its `file_node_idx` keeps. Each input's own `File` node sources its
    // records' file-level edges, as the index's `file_edges` do: the k-th
    // input of an indexed path takes the path's k-th base `File` node.
    let mut new_files: FxHashMap<&str, u32> = FxHashMap::default();
    let mut file_nodes: Vec<Option<u32>> = Vec::with_capacity(files.len());
    let mut inputs_of: FxHashMap<&str, usize> = FxHashMap::default();
    for file in files {
        let path = file.rel_path.as_str();
        let nth = inputs_of.entry(path).or_default();
        let k = *nth;
        *nth += 1;
        if indexed.contains(path) {
            let own = base_files
                .get(path)
                .and_then(|all| all.get(k).or(all.last()));
            file_nodes.push(own.copied());
            continue;
        }
        let idx = base_len + nodes.len() as u32;
        new_files.insert(path, idx);
        file_nodes.push(Some(idx));
        nodes.push(file_node(path));
    }
    replayed.defines_files = base_files.values().flatten().copied().collect();
    debug_assert!(
        new_files
            .iter()
            .all(|(path, idx)| dirty_files.get(*path) == Some(idx)),
        "the Imports edges start at the File nodes this replay numbers"
    );

    let mut push = |edge: ViewEdge| {
        debug_assert!(
            reason_index(edge.reason).is_some(),
            "VIEW_EDGE_REASONS must list every edge reason, or the view is never cached"
        );
        let ei = edges.len() as u32;
        edges.push(edge);
        out_adj.entry(edge.source).or_default().push(ei);
        in_adj.entry(edge.target).or_default().push(ei);
    };
    let defines = |source: u32, target: u32, reason: &'static str| ViewEdge {
        source,
        target,
        rel_type: RelType::Defines,
        confidence: 1.0,
        reason,
    };

    // A schema field's `HasProperty` shares the kind the view masks.
    let records_replay = !records.is_empty() && records.len() == files.len();
    // Name tables per path, built once for a path with a type or a variant
    // from the path's inputs, which are grouped once on the first such input.
    let mut declarations: FxHashMap<&str, PathDecls<'_>> = FxHashMap::default();
    let mut inputs_by_path: Option<FxHashMap<&str, Vec<FileOrd>>> = None;
    for (ord, file) in files.iter().enumerate() {
        let path = file.rel_path.as_str();
        let ord = FileOrd(ord as u32);
        let node = |raw: usize| raw_map.node(ord, RawIdx(raw as u32));

        // A path with no `File` node (only a hand-made graph has one) gets
        // no scope `Defines`, as the index skips it.
        let defining_file = base_files
            .get(path)
            .and_then(|all| all.last())
            .or_else(|| new_files.get(path))
            .copied();
        if let Some(file_idx) = defining_file {
            containment::scope_defines(&file.symbols, row, |scope, child| {
                let Some(child) = node(child).filter(|id| !lexical_parents.contains_key(id)) else {
                    return;
                };
                let source = match scope {
                    Scope::File => Some(file_idx),
                    Scope::Container(container) => node(container),
                };
                if let Some(source) = source {
                    push(defines(source, child, SCOPE_DEFINES_REASON));
                }
            });
        }

        if !file.symbols.iter().any(|s| reads_declarations(s.kind)) {
            continue;
        }
        let table = &*declarations.entry(path).or_insert_with(|| {
            let by_path = inputs_by_path.get_or_insert_with(|| {
                let mut by_path: FxHashMap<&str, Vec<FileOrd>> = FxHashMap::default();
                for (ord, file) in files.iter().enumerate() {
                    by_path
                        .entry(file.rel_path.as_str())
                        .or_default()
                        .push(FileOrd(ord as u32));
                }
                by_path
            });
            let ords = by_path.get(path).map(Vec::as_slice).unwrap_or_default();
            PathDecls::of(files, ords, &raw_map, nodes)
        });
        containment::enum_variant_defines(
            &file.symbols,
            row,
            |name, kind, owner| table.first(name, kind, owner),
            |source, target| push(defines(source, target, ENUM_VARIANT_DEFINES_REASON)),
        );
        let lookup = FileMembers {
            decls: table,
            later_overloads,
            raw_map: &raw_map,
            ord,
            lexical_parents,
        };
        membership::class_membership(
            &file.symbols,
            member_row,
            || lookup,
            |m| {
                if m.rel_type == RelType::HasProperty && !records_replay {
                    return;
                }
                push(ViewEdge {
                    source: m.source,
                    target: m.target,
                    rel_type: m.rel_type,
                    confidence: 1.0,
                    reason: m.reason,
                });
            },
        );
    }
    imports.into_iter().for_each(&mut push);

    if !records_replay {
        return replayed;
    }
    let first_record = nodes.len();
    let mut tables = Tables::new(graph, dirty_base, files, records, &raw_map);
    let context = RecordContext {
        files,
        records,
        base_len,
        raw_map: &raw_map,
        file_nodes: &file_nodes,
    };
    replay_records(&context, &mut tables, nodes, &mut push);
    let (replaced, suppressed) = pair_records(graph, &base_records, base_len, first_record, nodes);
    replayed.replaced = replaced;
    replayed.suppressed = suppressed;
    replayed.records_replayed = true;
    replayed
}

/// What [`replay_records`] reads besides the tables.
struct RecordContext<'a> {
    files: &'a [OverlayFileInput],
    records: &'a [OverlayFileRecords],
    base_len: u32,
    raw_map: &'a RawIndexMap,
    /// Each input's own `File` node; `None` for an indexed path the base
    /// holds no `File` node of.
    file_nodes: &'a [Option<u32>],
}

/// The record nodes and edges of every input, by the index's derivations
/// ([`record_nodes`]), in input order and, per input, in the index's pass
/// order. An endpoint that resolves to nothing gets no edge; a schema field
/// and a transaction scope without one get no node either, as in the index.
fn replay_records<'a>(
    context: &RecordContext<'a>,
    tables: &mut Tables<'a>,
    nodes: &mut Vec<ViewNode>,
    push: &mut impl FnMut(ViewEdge),
) {
    let RecordContext {
        files,
        records,
        base_len,
        raw_map,
        file_nodes,
    } = *context;
    for (ord, (file, records)) in files.iter().zip(records).enumerate() {
        let path = file.rel_path.as_str();
        let file_ord = FileOrd(ord as u32);
        let rel_path: Arc<str> = Arc::from(path);
        let own_file = file_nodes[ord];
        let source = move |endpoint: Option<&RawEndpoint>| -> Option<u32> {
            match raw_map.resolve(file_ord, endpoint?)? {
                GlobalEndpoint::Node(slot) => Some(slot),
                GlobalEndpoint::File(_) => own_file,
                GlobalEndpoint::Unresolved(_) => None,
            }
        };
        let mut add = |node: RecordNode<'_>| -> u32 {
            let idx = base_len + nodes.len() as u32;
            nodes.push(record_view_node(node, &rel_path));
            idx
        };
        let node_of = move |raw: RawIdx| {
            file.symbols
                .get(raw.index())
                .map(|s| (s.owner_class.as_deref(), s.name.as_str()))
        };

        for field in &records.schema_fields {
            let Some(GlobalEndpoint::Node(class)) = field
                .endpoint
                .as_ref()
                .and_then(|endpoint| raw_map.resolve(file_ord, endpoint))
            else {
                continue;
            };
            let to = add(record_nodes::schema_field_node(path, field));
            push(record_edge(
                class,
                to,
                RelType::HasProperty,
                SCHEMA_FIELD_HAS_PROPERTY_REASON,
            ));
        }

        record_nodes::event_topic_nodes(path, &records.event_topics, |i, node| {
            let topic = &records.event_topics[i];
            let to = add(node);
            if let Some(from) = source(topic.endpoint.as_ref()) {
                let (rel, reason) = record_nodes::event_topic_edge(topic.direction);
                push(record_edge(from, to, rel, reason));
            }
        });

        for scope in &records.tx_scopes {
            let raw = RawIdx(scope.node_idx());
            let (Some(callable), Some(from)) =
                (file.symbols.get(raw.index()), raw_map.node(file_ord, raw))
            else {
                continue;
            };
            let span = HeritageNode::span(callable);
            let node = record_nodes::tx_scope_node(path, *scope, &callable.name, span);
            let to = add(node);
            push(record_edge(
                from,
                to,
                RelType::OpensTxScope,
                TX_SCOPE_REASON,
            ));
        }

        // The index's `path_literal_nodes` derives the same table from the
        // same rows; a file with no literal pays nothing.
        let parents = match records.path_literals.is_empty() {
            true => Vec::new(),
            false => record_nodes::raw_lexical_parents(
                path,
                file.symbols.iter().map(|s| (s.kind, HeritageNode::span(s))),
            ),
        };
        for literal in &records.path_literals {
            let endpoint = literal.endpoint.as_ref();
            let to = add(record_nodes::path_literal_node(
                path, literal, endpoint, &parents, node_of,
            ));
            if let Some(from) = source(endpoint) {
                let reason = sink_reason(&literal.sink_reason);
                push(record_edge(from, to, RelType::UsesPathLiteral, reason));
            }
        }

        for sql in &records.sql_refs {
            let Some(from) = source(sql.endpoint.as_ref()) else {
                continue;
            };
            for (table, reason) in record_nodes::sql_table_refs(sql) {
                if let Some(to) = tables.target(table) {
                    push(record_edge(from, to, RelType::QueriesTable, reason));
                }
            }
        }
    }
}

/// Pair each base record node (`base_records`, ascending) with the replayed
/// node of its uid at `first_record..`, the k-th base with the k-th replayed
/// node in order, and mark the pair on the virtual node. Returns the pairs
/// and the base nodes left over. A `SchemaField` never pairs (module doc).
fn pair_records(
    graph: &ArchivedZeroCopyGraph,
    base_records: &[u32],
    base_len: u32,
    first_record: usize,
    nodes: &mut [ViewNode],
) -> (Vec<(u32, u32)>, Vec<u32>) {
    let mut by_uid: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    for (offset, node) in nodes.iter().enumerate().skip(first_record) {
        if node.kind != NodeKind::SchemaField {
            by_uid
                .entry(node.uid)
                .or_default()
                .push(base_len + offset as u32);
        }
    }
    // Popped from the back: the first replayed node pairs first.
    by_uid.values_mut().for_each(|virtuals| virtuals.reverse());
    let mut replaced = Vec::new();
    let mut suppressed = Vec::new();
    for &base in base_records {
        let node = &graph.nodes[base as usize];
        let twin = (NodeKind::from(&node.kind) != NodeKind::SchemaField)
            .then(|| by_uid.get_mut(&node.uid.to_native())?.pop())
            .flatten();
        match twin {
            Some(virt) => {
                nodes[(virt - base_len) as usize].replaced_base = Some(base);
                replaced.push((base, virt));
            }
            None => suppressed.push(base),
        }
    }
    (replaced, suppressed)
}

/// The `Class` node a `QueriesTable` edge targets for a table name: the one
/// the index's `sql_table_edges` picks, the first `Class` so named in a
/// fresh build's node order (files by path, nodes in file order, any
/// language). A base node of a dirty or removed path is out; the dirty
/// files' own declarations stand in, a tombstone excluded, as the index
/// gives it no name. Answers are memoized per name.
struct Tables<'a> {
    graph: &'a ArchivedZeroCopyGraph,
    dirty_base: &'a FxHashSet<u32>,
    /// Per name, the first virtual `Class` by (path, index).
    dirty: FxHashMap<&'a str, (&'a str, u32)>,
    memo: FxHashMap<&'a str, Option<u32>>,
}

impl<'a> Tables<'a> {
    /// Empty, at no cost, when no input names a table.
    fn new(
        graph: &'a ArchivedZeroCopyGraph,
        dirty_base: &'a FxHashSet<u32>,
        files: &'a [OverlayFileInput],
        records: &[OverlayFileRecords],
        raw_map: &RawIndexMap,
    ) -> Self {
        let mut dirty: FxHashMap<&'a str, (&'a str, u32)> = FxHashMap::default();
        if records.iter().any(|r| !r.sql_refs.is_empty()) {
            for (ord, file) in files.iter().enumerate() {
                let path = file.rel_path.as_str();
                for (raw, symbol) in file.symbols.iter().enumerate() {
                    if symbol.kind != NodeKind::Class {
                        continue;
                    }
                    let raw = RawIdx(raw as u32);
                    let ord = FileOrd(ord as u32);
                    // A tombstone is no table: the index clears its name.
                    let slot = raw_map.slot(ord, raw);
                    let Some(slot) = slot.filter(|&s| raw_map.node(ord, raw) == Some(s)) else {
                        continue;
                    };
                    let candidate = (path, slot);
                    dirty
                        .entry(symbol.name.as_str())
                        .and_modify(|best| {
                            if earlier(candidate, *best) {
                                *best = candidate;
                            }
                        })
                        .or_insert(candidate);
                }
            }
        }
        Self {
            graph,
            dirty_base,
            dirty,
            memo: FxHashMap::default(),
        }
    }

    fn target(&mut self, table: &'a str) -> Option<u32> {
        if let Some(&known) = self.memo.get(table) {
            return known;
        }
        let graph = self.graph;
        let pool = &graph.string_pool;
        let base = graph
            .nodes_by_name(table)
            .filter(|idx| !self.dirty_base.contains(idx))
            .filter(|&idx| NodeKind::from(&graph.nodes[idx as usize].kind) == NodeKind::Class)
            .filter_map(|idx| {
                let file = graph.nodes[idx as usize].file_idx.to_native() as usize;
                Some((graph.files.get(file)?.path.resolve(pool), idx))
            })
            .reduce(|a, b| if earlier(b, a) { b } else { a });
        let pick = match (base, self.dirty.get(table).copied()) {
            (Some(b), Some(d)) => Some(if earlier(d, b) { d } else { b }),
            (b, d) => b.or(d),
        }
        .map(|(_, idx)| idx);
        self.memo.insert(table, pick);
        pick
    }
}

/// Whether `a` comes before `b` in a fresh build's node order: files sort
/// by `Path` (component-wise, as `GraphBuilder::build` sorts them), nodes
/// by index within one path.
fn earlier(a: (&str, u32), b: (&str, u32)) -> bool {
    (Path::new(a.0), a.1) < (Path::new(b.0), b.1)
}

fn record_edge(source: u32, target: u32, rel_type: RelType, reason: &'static str) -> ViewEdge {
    ViewEdge {
        source,
        target,
        rel_type,
        confidence: 1.0,
        reason,
    }
}

/// A record node as the view holds it: 1-based lines from the 0-based span.
fn record_view_node(node: RecordNode<'_>, rel_path: &Arc<str>) -> ViewNode {
    ViewNode {
        uid: node.uid,
        owner_class: (!node.owner.is_empty()).then(|| node.owner.to_owned()),
        name: node.name.into_owned(),
        kind: node.kind,
        rel_path: rel_path.clone(),
        start_line: node.span.0 + 1,
        end_line: node.span.2 + 1,
        start_column: node.span.1,
        end_column: node.span.3,
        replaced_base: None,
        later_overload: false,
    }
}

/// `reason` as a `'static` str: the [`PATH_LITERAL_SINK_REASONS`] entry it
/// equals, else one interned copy per distinct text. A release build so
/// keeps the edge of a reason an extractor added without listing it; the
/// view then never enters the cache, and a debug build fails the reason
/// assertion of [`replay`]'s `push`.
fn sink_reason(reason: &str) -> &'static str {
    if let Some(&known) = PATH_LITERAL_SINK_REASONS.iter().find(|&&r| r == reason) {
        return known;
    }
    static OTHER: OnceLock<Mutex<FxHashSet<&'static str>>> = OnceLock::new();
    let mut other = OTHER
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(&interned) = other.get(reason) {
        return interned;
    }
    let interned: &'static str = Box::leak(reason.into());
    other.insert(interned);
    interned
}

fn row(symbol: &super::view::OverlaySymbol) -> Row<'_> {
    Row {
        kind: symbol.kind,
        owner: symbol.owner_class.as_deref(),
        name: &symbol.name,
        span: HeritageNode::span(symbol),
    }
}

/// The index's `File` node of `path`: the uid of `GraphBuilder::build`, the
/// base name for its name (the whole path when it has none), line 1.
fn file_node(path: &str) -> ViewNode {
    let name = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());
    ViewNode {
        uid: crate::uid::compute(NodeKind::File, path, None, path),
        name,
        kind: NodeKind::File,
        owner_class: None,
        rel_path: Arc::from(path),
        start_line: 1,
        end_line: 1,
        start_column: 0,
        end_column: 0,
        replaced_base: None,
        later_overload: false,
    }
}

fn member_row(symbol: &super::view::OverlaySymbol) -> MemberRow<'_> {
    MemberRow {
        kind: symbol.kind,
        owner: symbol.owner_class.as_deref(),
        name: &symbol.name,
        span: HeritageNode::span(symbol),
        arity: symbol.generic_arity,
    }
}

/// Whether a declaration of `kind` makes its path's [`PathDecls`] worth
/// building: a type owns members, an enum variant is contained.
fn reads_declarations(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Interface
            | NodeKind::Annotation
            | NodeKind::Enum
            | NodeKind::EnumVariant
    )
}

/// One path's declarations as the index's `SymbolTable` holds them after
/// Pass 1, over the path's inputs in input order. Only a registered
/// declaration is in the name table: a tombstone, a later overload and a C#
/// arity twin are in none, as the index registers none of them by name.
struct PathDecls<'a> {
    /// Name → registered slots, in input order: the index's `file_scoped`.
    by_name: FxHashMap<&'a str, Vec<u32>>,
    /// Registered slot → (kind, owner key).
    decl: FxHashMap<u32, (NodeKind, Option<&'a str>)>,
    /// First slot → its arity twins: `SymbolTable::register_arity_sibling`.
    siblings: FxHashMap<u32, Vec<u32>>,
    /// Every arity twin slot.
    twins: FxHashSet<u32>,
    /// The declared generic arity of a C# type and of each arity twin, when
    /// not 0: `SymbolTable::note_type_arity`.
    arities: FxHashMap<u32, u32>,
}

impl<'a> PathDecls<'a> {
    /// The tables of the inputs `ords` of one path, in input order.
    fn of(
        files: &'a [OverlayFileInput],
        ords: &[FileOrd],
        raw_map: &RawIndexMap,
        nodes: &[ViewNode],
    ) -> Self {
        let mut table = Self {
            by_name: FxHashMap::default(),
            decl: FxHashMap::default(),
            siblings: FxHashMap::default(),
            twins: FxHashSet::default(),
            arities: FxHashMap::default(),
        };
        for &ord in ords {
            let file = &files[ord.0 as usize];
            let language = file.file_meta().language;
            for (raw, symbol) in file.symbols.iter().enumerate() {
                let raw = RawIdx(raw as u32);
                let (Some(slot), Some(node)) = (raw_map.slot(ord, raw), raw_map.node(ord, raw))
                else {
                    continue;
                };
                if slot != node || nodes[(slot - raw_map.base()) as usize].later_overload {
                    continue;
                }
                table
                    .by_name
                    .entry(symbol.name.as_str())
                    .or_default()
                    .push(slot);
                let owner = symbol.owner_class.as_deref().and_then(owner_key);
                table.decl.insert(slot, (symbol.kind, owner));
                if splits_by_arity(language, symbol.kind) && symbol.generic_arity > 0 {
                    table.arities.insert(slot, symbol.generic_arity);
                }
            }
            if language == Language::CSharp {
                table.note_twins(file, ord, language, raw_map);
            }
        }
        table
    }

    /// Note the arity twins of one C# input, numbered as the view numbers
    /// them ([`file_identities`]): a `Later` slot that
    /// [`later_is_arity_twin`] calls a twin, filed beside its first.
    fn note_twins(
        &mut self,
        file: &OverlayFileInput,
        ord: FileOrd,
        language: Language,
        raw_map: &RawIndexMap,
    ) {
        let identities = file_identities(&file.rel_path, &file.symbols, |sym| {
            (
                sym.kind,
                sym.owner_class.as_deref(),
                sym.name.as_str(),
                HeritageNode::span(sym),
                sym.generic_arity,
            )
        });
        for (raw, identity) in identities.into_iter().enumerate() {
            let UidSlot::Later { first, .. } = identity.slot else {
                continue;
            };
            let symbol = &file.symbols[raw];
            if !later_is_arity_twin(language, symbol.kind) {
                continue;
            }
            let (Some(twin), Some(first)) = (
                raw_map.node(ord, RawIdx(raw as u32)),
                raw_map.node(ord, RawIdx(first)),
            ) else {
                continue;
            };
            self.siblings.entry(first).or_default().push(twin);
            self.twins.insert(twin);
            if symbol.generic_arity > 0 {
                self.arities.insert(twin, symbol.generic_arity);
            }
        }
    }

    /// The first registered declaration of (name, kind, owner key): the
    /// index's `SymbolTable::first_declaration`.
    fn first(&self, name: &str, kind: NodeKind, owner: Option<&str>) -> Option<u32> {
        let owner = owner.and_then(owner_key);
        self.by_name.get(name)?.iter().copied().find(|slot| {
            self.decl
                .get(slot)
                .is_some_and(|&(k, o)| k == kind && o == owner)
        })
    }
}

/// Class membership's lookups for one input: its path's [`PathDecls`], the
/// view's overload sets, and the input's own slots.
struct FileMembers<'t, 'a> {
    decls: &'t PathDecls<'a>,
    later_overloads: &'t FxHashMap<u32, Vec<u32>>,
    raw_map: &'t RawIndexMap,
    ord: FileOrd,
    lexical_parents: &'t FxHashMap<u32, u32>,
}

impl MemberLookup for FileMembers<'_, '_> {
    fn first_declaration(&self, name: &str, kind: NodeKind, owner: Option<&str>) -> Option<u32> {
        self.decls.first(name, kind, owner)
    }

    fn lookup_in_file(&self, name: &str) -> Option<u32> {
        self.decls.by_name.get(name)?.first().copied()
    }

    fn later_overloads(&self, first: u32) -> &[u32] {
        self.later_overloads.get(&first).map_or(&[], Vec::as_slice)
    }

    /// `SymbolTable::with_arity` over the twins of `id`.
    fn with_arity(&self, id: u32, arity: u32) -> Option<u32> {
        let type_arity = |member: u32| self.decls.arities.get(&member).copied().unwrap_or(0);
        match self.decls.siblings.get(&id) {
            Some(twins) => std::iter::once(id)
                .chain(twins.iter().copied())
                .find(|&member| type_arity(member) == arity),
            None => (arity == 0 || type_arity(id) == arity).then_some(id),
        }
    }

    fn is_scoped(&self, raw: usize) -> bool {
        self.raw_map
            .node(self.ord, RawIdx(raw as u32))
            .is_some_and(|id| self.lexical_parents.contains_key(&id))
    }

    fn twin(&self, raw: usize) -> Option<u32> {
        self.raw_map
            .node(self.ord, RawIdx(raw as u32))
            .filter(|slot| self.decls.twins.contains(slot))
    }
}

/// The function meta of every virtual slot whose fragment paired one, as
/// pass 1.8 records it for the slot (a tombstone slot included).
fn function_metas(files: &[OverlayFileInput], base_len: u32) -> Vec<ViewFunctionMeta> {
    (base_len..)
        .zip(files.iter().flat_map(|f| &f.symbols))
        .filter_map(|(node, symbol)| {
            Some(ViewFunctionMeta {
                node,
                flags: symbol.flags?,
                return_type: symbol.return_type.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::types::RawImport;
    use crate::graph::{ArchivedZeroCopyGraph, FunctionMeta};
    use crate::graph_fixture::GraphFixture;
    use crate::session::merged::MergedGraph;
    use crate::session::view::{OverlaySymbol, OverlayView};
    use rkyv::rancor::Error as RkyvError;

    fn sym(kind: NodeKind, owner: Option<&str>, name: &str, lines: (u32, u32)) -> OverlaySymbol {
        OverlaySymbol {
            is_exported: false,
            name: name.to_string(),
            kind,
            owner_class: owner.map(str::to_string),
            start_line: lines.0,
            end_line: lines.1,
            start_column: 0,
            end_column: 0,
            calls: Vec::new(),
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
        }
    }

    fn func(name: &str, lines: (u32, u32)) -> OverlaySymbol {
        sym(NodeKind::Function, None, name, lines)
    }

    fn input(path: &str, symbols: Vec<OverlaySymbol>) -> OverlayFileInput {
        OverlayFileInput {
            parsed_language: None,
            rel_path: path.to_string(),
            symbols,
            imports: Vec::new(),
            export_aliases: Vec::new(),
        }
    }

    /// One clean file, so a virtual index never collides with base 0.
    fn clean_base() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        fx.func("src/clean.rs", "clean_fn");
        fx.into_bytes()
    }

    fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
        rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(bytes).unwrap()
    }

    /// Every `Defines` edge of the merged graph as (source, target, reason).
    fn defines(graph: &ArchivedZeroCopyGraph, view: &OverlayView) -> Vec<(u32, u32, String)> {
        let merged = MergedGraph::new(graph, Some(view));
        let mut out: Vec<(u32, u32, String)> = merged
            .all_edges()
            .filter(|e| e.rel_type() == RelType::Defines)
            .map(|e| (e.source, e.target, e.reason(graph).to_string()))
            .collect();
        out.sort();
        out
    }

    fn scope(source: u32, target: u32) -> (u32, u32, String) {
        (source, target, SCOPE_DEFINES_REASON.to_string())
    }

    /// The virtual `File` nodes of `view`, as (index, name, path).
    fn file_nodes(view: &OverlayView) -> Vec<(u32, String, String)> {
        (view.base_len()..)
            .zip(view.virtual_nodes())
            .filter(|(_, n)| n.kind == NodeKind::File)
            .map(|(idx, n)| (idx, n.name.clone(), n.rel_path.to_string()))
            .collect()
    }

    /// `#include "header"` as the C parser records it.
    fn include(header: &str) -> RawImport {
        RawImport {
            source: format!("\"{header}\""),
            imported_name: "*".to_string(),
            alias: None,
            binding_kind: None,
        }
    }

    /// Every `Imports` edge of the merged graph as (source, target,
    /// confidence, reason).
    fn imports(graph: &ArchivedZeroCopyGraph, view: &OverlayView) -> Vec<(u32, u32, f32, String)> {
        let merged = MergedGraph::new(graph, Some(view));
        let mut out: Vec<(u32, u32, f32, String)> = merged
            .all_edges()
            .filter(|e| e.rel_type() == RelType::Imports)
            .map(|e| {
                (
                    e.source,
                    e.target,
                    e.confidence(),
                    e.reason(graph).to_string(),
                )
            })
            .collect();
        out.sort_by_key(|e| (e.0, e.1));
        out
    }

    /// A dirty C file swaps `alpha.h` for `beta.h`: the base `Imports` edge
    /// of its `File` node is masked, and the replayed one starts at that
    /// node with the index's guess confidence and reason (step 3c).
    #[test]
    fn test_replay_dirty_file_imports_start_at_base_file_and_mask_base_ones() {
        let mut fx = GraphFixture::new();
        let main = fx.node(NodeKind::File, "src/main.c", "main.c");
        let alpha = fx.node(NodeKind::File, "include/alpha.h", "alpha.h");
        let beta = fx.node(NodeKind::File, "include/beta.h", "beta.h");
        fx.edge_with(
            main,
            alpha,
            RelType::Imports,
            0.6,
            "post_process:imports:guess",
        );
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let mut file = input("src/main.c", vec![func("run", (3, 3))]);
        file.imports = vec![include("beta.h")];
        let view = OverlayView::build(graph, &[file]).unwrap();

        assert!(view.masks_base_edge(main, RelType::Imports));
        assert_eq!(
            imports(graph, &view),
            [(main, beta, 0.6, "post_process:imports:guess".to_string())]
        );
    }

    /// A new file has no base `File` node: its `Imports` edges start at the
    /// virtual one the replay appends, after the file's declarations.
    #[test]
    fn test_replay_new_file_imports_start_at_its_virtual_file_node() {
        let mut fx = GraphFixture::new();
        let alpha = fx.node(NodeKind::File, "include/alpha.h", "alpha.h");
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let mut file = input("src/new.c", vec![func("run", (3, 3))]);
        file.imports = vec![include("alpha.h")];
        let view = OverlayView::build(graph, &[file]).unwrap();
        let new_file = view.base_len() + 1;

        assert_eq!(
            file_nodes(&view),
            [(new_file, "new.c".to_string(), "src/new.c".to_string())]
        );
        assert_eq!(
            imports(graph, &view),
            [(
                new_file,
                alpha,
                0.6,
                "post_process:imports:guess".to_string()
            )]
        );
    }

    /// An include of a header no file has adds no edge, and the base one
    /// stays masked: the dirty file imports nothing.
    #[test]
    fn test_replay_import_of_missing_header_emits_nothing() {
        let mut fx = GraphFixture::new();
        let main = fx.node(NodeKind::File, "src/main.c", "main.c");
        let alpha = fx.node(NodeKind::File, "include/alpha.h", "alpha.h");
        fx.edge_with(
            main,
            alpha,
            RelType::Imports,
            0.6,
            "post_process:imports:guess",
        );
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let mut file = input("src/main.c", Vec::new());
        file.imports = vec![include("gone.h")];
        let view = OverlayView::build(graph, &[file]).unwrap();

        assert!(view.masks_base_edge(main, RelType::Imports));
        assert!(imports(graph, &view).is_empty());
    }

    /// An include of a header the worktree removed binds nothing, as the
    /// index's file set no longer has it.
    #[test]
    fn test_replay_import_of_removed_header_emits_nothing() {
        let mut fx = GraphFixture::new();
        fx.node(NodeKind::File, "src/main.c", "main.c");
        fx.node(NodeKind::File, "include/alpha.h", "alpha.h");
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let mut file = input("src/main.c", Vec::new());
        file.imports = vec![include("alpha.h")];
        let view =
            OverlayView::build_with_removed(graph, &[file], &["include/alpha.h".to_string()])
                .unwrap();

        assert!(imports(graph, &view).is_empty());
    }

    /// The table numbers a new path's `File` node as the replay does: the
    /// last input of the path, after every earlier new input; an indexed
    /// path takes its last base `File` node, or none.
    #[test]
    fn test_dirty_file_nodes_numbers_new_paths_in_input_order() {
        let files = [
            input("src/a.rs", Vec::new()),
            input("src/kept.rs", Vec::new()),
            input("src/a.rs", Vec::new()),
            input("src/b.rs", Vec::new()),
            input("src/bare.rs", Vec::new()),
        ];
        let indexed: FxHashSet<&str> = ["src/kept.rs", "src/bare.rs"].into_iter().collect();
        let base_files: FxHashMap<&str, Vec<u32>> =
            [("src/kept.rs", vec![4, 9])].into_iter().collect();
        let table = dirty_file_nodes(&files, &indexed, &base_files, 100);
        let mut rows: Vec<(&str, u32)> = table.into_iter().collect();
        rows.sort();
        assert_eq!(
            rows,
            [("src/a.rs", 101), ("src/b.rs", 102), ("src/kept.rs", 9)]
        );
    }

    #[test]
    fn test_replay_new_file_gets_file_node_and_defines_edge_per_top_level_declaration() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/new.rs",
            vec![func("a", (1, 2)), func("b", (4, 5))],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();

        let file = base + 2;
        assert_eq!(
            file_nodes(&view),
            [(file, "new.rs".to_string(), "src/new.rs".to_string())]
        );
        let node = view.node(file).unwrap();
        assert_eq!(
            node.uid,
            crate::uid::compute(NodeKind::File, "src/new.rs", None, "src/new.rs"),
            "the index's File uid"
        );
        assert_eq!(
            (node.start_line, node.end_line, node.owner_class.as_deref()),
            (1, 1, None)
        );
        assert_eq!(
            defines(graph, &view),
            [scope(file, base), scope(file, base + 1)]
        );
        let edge = view.overlay_out(file).next().unwrap().1;
        assert_eq!(edge.confidence, 1.0);
    }

    #[test]
    fn test_replay_new_empty_file_has_file_node_and_no_defines() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let view = OverlayView::build(graph, &[input("src/empty.ts", Vec::new())]).unwrap();
        assert_eq!(
            file_nodes(&view),
            [(
                view.base_len(),
                "empty.ts".to_string(),
                "src/empty.ts".to_string()
            )]
        );
        assert!(defines(graph, &view).is_empty());
    }

    /// A file with only imports declares nothing: its `File` node stands,
    /// with no `Defines`.
    #[test]
    fn test_replay_new_file_with_only_imports_has_file_node_and_no_defines() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let mut only_imports = input("src/reexport.ts", Vec::new());
        only_imports
            .imports
            .push(crate::analyzer::types::RawImport {
                source: "./lib".to_string(),
                imported_name: "helper".to_string(),
                alias: None,
                binding_kind: None,
            });
        let view = OverlayView::build(graph, &[only_imports]).unwrap();
        assert_eq!(file_nodes(&view).len(), 1);
        assert!(defines(graph, &view).is_empty());
    }

    /// The base `File` node of a re-parsed path sources the rebuilt edges,
    /// and its stale base `Defines` (to a deleted and a kept declaration)
    /// are masked: each declaration has exactly one parent.
    #[test]
    fn test_replay_dirty_file_defines_start_at_base_file_and_mask_base_ones() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/dirty.rs", "dirty.rs");
        let keep = fx.func("src/dirty.rs", "keep_fn");
        let gone = fx.func("src/dirty.rs", "gone_fn");
        fx.edge(file, keep, RelType::Defines);
        fx.edge(file, gone, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [input(
            "src/dirty.rs",
            vec![func("keep_fn", (1, 2)), func("new_fn", (4, 5))],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();

        assert!(file_nodes(&view).is_empty(), "the base File node stays");
        assert_eq!(view.redirect(file), Some(file));
        assert!(view.masks_base_edge(file, RelType::Defines));
        assert!(view.masks_base_edge(file, RelType::Imports));
        assert_eq!(
            defines(graph, &view),
            [scope(file, base), scope(file, base + 1)]
        );
    }

    /// An edit that empties the file leaves its `File` node with no
    /// `Defines`.
    #[test]
    fn test_replay_emptied_file_keeps_file_node_and_drops_every_defines() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/lib.ts", "lib.ts");
        let helper = fx.func("src/lib.ts", "helper");
        fx.edge(file, helper, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let view = OverlayView::build(graph, &[input("src/lib.ts", Vec::new())]).unwrap();
        assert_eq!(view.redirect(file), Some(file));
        assert!(defines(graph, &view).is_empty());
    }

    /// A removed file has no parse: none of its `Defines` survive, and the
    /// replay makes no `File` node for it.
    #[test]
    fn test_replay_removed_file_has_no_file_node_and_no_defines() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/gone.rs", "gone.rs");
        let gone = fx.func("src/gone.rs", "gone_fn");
        fx.edge(file, gone, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let view =
            OverlayView::build_with_removed(graph, &[], &["src/gone.rs".to_string()]).unwrap();
        assert!(view.virtual_nodes().is_empty());
        assert!(defines(graph, &view).is_empty());
    }

    /// `foo` moves from `a.rs` to `b.rs`, both dirty: only `b.rs` defines it.
    #[test]
    fn test_replay_declaration_moved_between_dirty_files_defined_by_new_file_only() {
        let mut fx = GraphFixture::new();
        let a = fx.node(NodeKind::File, "src/a.rs", "a.rs");
        let b = fx.node(NodeKind::File, "src/b.rs", "b.rs");
        let foo = fx.func("src/a.rs", "foo");
        fx.edge(a, foo, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [
            input("src/a.rs", Vec::new()),
            input("src/b.rs", vec![func("foo", (1, 2))]),
        ];
        let view = OverlayView::build(graph, &files).unwrap();
        assert_eq!(view.redirect(foo), None, "a.rs no longer declares it");
        assert_eq!(defines(graph, &view), [scope(b, view.base_len())]);
    }

    /// Two inputs of one new path make two `File` nodes, as two
    /// `LocalGraph`s do; both inputs' declarations hang off the last one.
    #[test]
    fn test_replay_two_inputs_of_one_new_path_define_from_last_file_node() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [
            input("src/two.vue", vec![func("first", (1, 2))]),
            input("src/two.vue", vec![func("second", (4, 5))]),
        ];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let files = file_nodes(&view);
        assert_eq!(files.len(), 2);
        let last = files[1].0;
        assert_eq!(
            defines(graph, &view),
            [scope(last, base), scope(last, base + 1)]
        );
    }

    /// `namespace Acme` twice: the index tombstones the second block, so the
    /// file defines the first one twice, and a member of the second block
    /// hangs off the first.
    #[test]
    fn test_replay_repeated_namespace_defines_survivor_twice_and_its_members() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/Lib.cs",
            vec![
                sym(NodeKind::Namespace, None, "Acme", (1, 9)),
                sym(NodeKind::Class, Some("Acme"), "Lib", (3, 8)),
                sym(NodeKind::Namespace, None, "Acme", (11, 19)),
                sym(NodeKind::Class, Some("Acme"), "Extra", (13, 18)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let file = base + 4;
        assert_eq!(
            view.node(base + 2).unwrap().name,
            "",
            "the second block is a tombstone"
        );
        assert_eq!(
            defines(graph, &view),
            [
                scope(base, base + 1),
                scope(base, base + 3),
                scope(file, base),
                scope(file, base),
            ]
        );
    }

    /// A Rust nested `fn` is scoped under its callable: no `Defines`.
    #[test]
    fn test_replay_scoped_nested_fn_gets_no_defines() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/n.rs",
            vec![func("outer", (1, 5)), func("inner", (2, 3))],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        assert_eq!(view.lexical_parent(base + 1), Some(base));
        assert_eq!(defines(graph, &view), [scope(base + 2, base)]);
    }

    #[test]
    fn test_replay_whitespace_only_name_is_defined_like_any_other() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let view =
            OverlayView::build(graph, &[input("src/w.rs", vec![func(" ", (1, 2))])]).unwrap();
        let base = view.base_len();
        assert_eq!(defines(graph, &view), [scope(base + 1, base)]);
    }

    /// Each enum defines its own variants; an enum with no variants defines
    /// none, and both enums hang off the file.
    #[test]
    fn test_replay_enum_variants_contained_and_empty_enum_defines_none() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/e.rs",
            vec![
                sym(NodeKind::Enum, None, "Color", (1, 4)),
                sym(NodeKind::EnumVariant, Some("Color"), "Red", (2, 2)),
                sym(NodeKind::EnumVariant, Some("Color"), "Green", (3, 3)),
                sym(NodeKind::Enum, None, "Empty", (5, 5)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let file = base + 4;
        let variant = |target| (base, target, ENUM_VARIANT_DEFINES_REASON.to_string());
        assert_eq!(
            defines(graph, &view),
            [
                variant(base + 1),
                variant(base + 2),
                scope(file, base),
                scope(file, base + 3)
            ]
        );
    }

    /// Two enums share a variant name: each variant pairs with its own enum
    /// by owner.
    #[test]
    fn test_replay_enums_sharing_a_variant_name_pair_by_owner() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/s.rs",
            vec![
                sym(NodeKind::Enum, None, "Shape", (1, 3)),
                sym(NodeKind::EnumVariant, Some("Shape"), "None", (2, 2)),
                sym(NodeKind::Enum, None, "Fill", (4, 6)),
                sym(NodeKind::EnumVariant, Some("Fill"), "None", (5, 5)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let pairs: Vec<(u32, u32)> = defines(graph, &view)
            .into_iter()
            .filter(|(_, _, reason)| reason == ENUM_VARIANT_DEFINES_REASON)
            .map(|(s, t, _)| (s, t))
            .collect();
        assert_eq!(pairs, [(base, base + 1), (base + 2, base + 3)]);
    }

    /// Two inputs of one path share its first declarations: the second
    /// input repeats the enum, and its variant pairs with the first input's
    /// enum, never with the same-name enum of another path. The inputs of
    /// each path are grouped once, before the first variant.
    #[test]
    fn test_replay_two_inputs_of_one_path_variant_pairs_enum_of_same_path() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [
            input(
                "src/a.rs",
                vec![
                    sym(NodeKind::Enum, None, "Color", (1, 3)),
                    sym(NodeKind::EnumVariant, Some("Color"), "Red", (2, 2)),
                ],
            ),
            input(
                "src/b.rs",
                vec![
                    sym(NodeKind::Enum, None, "Color", (1, 3)),
                    sym(NodeKind::EnumVariant, Some("Color"), "Blue", (2, 2)),
                ],
            ),
            // A second input of `src/a.rs` that repeats the enum: its first
            // declaration is the first input's.
            input(
                "src/a.rs",
                vec![
                    sym(NodeKind::Enum, None, "Color", (4, 6)),
                    sym(NodeKind::EnumVariant, Some("Color"), "Green", (5, 5)),
                ],
            ),
        ];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        let mut pairs: Vec<(u32, u32)> = defines(graph, &view)
            .into_iter()
            .filter(|(_, _, reason)| reason == ENUM_VARIANT_DEFINES_REASON)
            .map(|(s, t, _)| (s, t))
            .collect();
        pairs.sort_unstable();
        assert_eq!(
            pairs,
            [(base, base + 1), (base, base + 5), (base + 2, base + 3)]
        );
    }

    /// A dirty function's meta is the one its fragment paired, slot for
    /// slot; a function without one, and a base node, have none.
    #[test]
    fn test_replay_function_meta_carries_paired_flags_and_return_type() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let mut run = func("run", (1, 3));
        run.flags = Some(FunctionMeta::FLAG_ASYNC | FunctionMeta::FLAG_STATIC);
        run.return_type = Some("Promise<number>".to_string());
        let mut public = func("public_instance", (5, 6));
        public.flags = Some(0);
        let files = [input("src/f.ts", vec![run, func("plain", (8, 9)), public])];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();

        assert_eq!(
            view.function_meta(base),
            Some(&ViewFunctionMeta {
                node: base,
                flags: FunctionMeta::FLAG_ASYNC | FunctionMeta::FLAG_STATIC,
                return_type: Some("Promise<number>".to_string()),
            })
        );
        assert_eq!(view.function_meta(base + 1), None, "no meta paired");
        assert_eq!(
            view.function_meta(base + 2).map(|m| m.flags),
            Some(0),
            "zero flags are a value, not absence"
        );
        assert_eq!(view.function_meta(0), None, "a base node");
        assert_eq!(view.function_meta(base + 3), None, "the File node");
    }

    /// Two refreshes of one session build equal views and equal cache
    /// bytes, and the bytes decode to the same view, the base-sourced
    /// `Defines` and the function meta included.
    #[test]
    fn test_replay_same_inputs_twice_build_equal_views_and_cache_bytes() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/dirty.rs", "dirty.rs");
        fx.func("src/dirty.rs", "keep_fn");
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let mut keep = func("keep_fn", (1, 2));
        keep.flags = Some(FunctionMeta::FLAG_TEST);
        let files = [
            input("src/dirty.rs", vec![keep]),
            input(
                "src/new.rs",
                vec![
                    sym(NodeKind::Enum, None, "E", (1, 3)),
                    sym(NodeKind::EnumVariant, Some("E"), "A", (2, 2)),
                ],
            ),
        ];
        let first = OverlayView::build(graph, &files).unwrap();
        let second = OverlayView::build(graph, &files).unwrap();
        assert_eq!(first, second);
        let cached = first.to_cache_bytes();
        assert!(!cached.is_empty(), "every reason is listed");
        assert_eq!(cached, second.to_cache_bytes());
        let decoded = OverlayView::from_cache_bytes(&cached, graph.nodes.len()).unwrap();
        assert_eq!(decoded, first);
        assert_eq!(
            decoded.overlay_out(file).count(),
            1,
            "the base File node's rebuilt edge decodes"
        );
        assert!(decoded.masks_base_edge(file, RelType::Defines));
        assert!(decoded.function_meta(decoded.base_len()).is_some());
    }

    // ── record outputs ───────────────────────────────────────────────────

    use crate::analyzer::record_nodes::{EVENT_TOPIC_PUBLISHES_REASON, PATH_LITERAL_SINK_REASONS};
    use crate::analyzer::types::{
        FrameworkId, PubSub, RawEventTopic, RawPathLiteral, RawSchemaField, RawSqlRef, RawTxScope,
        SchemaType, SqlVerb,
    };
    use crate::contracts::raw_records::{RawEndpoint, RawIdx};
    use crate::session::view::OverlayFileRecords;

    const SINK: &str = PATH_LITERAL_SINK_REASONS[0];

    fn literal(value: &str, row: u32, endpoint: RawEndpoint) -> RawPathLiteral {
        RawPathLiteral {
            value: value.to_string(),
            span: (row, 0, row, 8),
            enclosing_symbol: None,
            enclosing_owner: None,
            sink_reason: SINK.to_string(),
            endpoint: Some(endpoint),
        }
    }

    fn literals(items: Vec<RawPathLiteral>) -> OverlayFileRecords {
        OverlayFileRecords {
            path_literals: items,
            ..OverlayFileRecords::default()
        }
    }

    fn sql(tables: &[(&str, SqlVerb)], endpoint: RawEndpoint) -> RawSqlRef {
        RawSqlRef {
            tables: tables.iter().map(|&(t, v)| (t.to_string(), v)).collect(),
            unresolved: false,
            span: (1, 4, 1, 40),
            enclosing_symbol: None,
            enclosing_owner: None,
            endpoint: Some(endpoint),
        }
    }

    fn topic(literal: Option<&str>, row: u32) -> RawEventTopic {
        RawEventTopic {
            topic_literal: literal.map(Into::into),
            topic_key: literal.map(Into::into),
            direction: PubSub::Publish,
            lib: FrameworkId::Kafka,
            enclosing_fn: "publish".into(),
            span: (row, 0, row, 9),
            endpoint: Some(RawEndpoint::Node(RawIdx(0))),
        }
    }

    /// The view with `records`, held to its cache round trip: every edge
    /// reason the replay writes is listed, so the view is stored.
    fn with_records(
        graph: &ArchivedZeroCopyGraph,
        files: &[OverlayFileInput],
        records: &[OverlayFileRecords],
    ) -> OverlayView {
        let view = OverlayView::build_with_records(graph, files, records, &[]).unwrap();
        let cached = view.to_cache_bytes();
        assert!(!cached.is_empty(), "every reason is listed");
        assert_eq!(
            OverlayView::from_cache_bytes(&cached, graph.nodes.len()).as_ref(),
            Some(&view),
            "cache round trip"
        );
        view
    }

    /// Every merged edge of `rel` as (source, target, reason), sorted.
    fn rel_edges(
        graph: &ArchivedZeroCopyGraph,
        view: &OverlayView,
        rel: RelType,
    ) -> Vec<(u32, u32, String)> {
        let merged = MergedGraph::new(graph, Some(view));
        let mut out: Vec<(u32, u32, String)> = merged
            .all_edges()
            .filter(|e| e.rel_type() == rel)
            .map(|e| (e.source, e.target, e.reason(graph).to_string()))
            .collect();
        out.sort();
        out
    }

    fn virtual_of(view: &OverlayView, kind: NodeKind) -> Vec<u32> {
        (view.base_len()..)
            .zip(view.virtual_nodes())
            .filter(|(_, n)| n.kind == kind)
            .map(|(idx, _)| idx)
            .collect()
    }

    fn only(nodes: Vec<u32>) -> u32 {
        assert_eq!(nodes.len(), 1, "{nodes:?}");
        nodes[0]
    }

    /// A base file with one path literal on row 3 (line 4), sourced at the
    /// `File` node: (bytes, File, PathLiteral).
    fn php_base() -> (Vec<u8>, u32, u32) {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/main.php", "main.php");
        let lit = fx.node(NodeKind::PathLiteral, "src/main.php", "/Lib.php");
        fx.span(lit, (3, 0, 3, 8));
        fx.edge_with(file, lit, RelType::UsesPathLiteral, 1.0, SINK);
        (fx.into_bytes(), file, lit)
    }

    /// An edit that moves the literal up one line: the replayed node keeps
    /// the uid, takes the new line, and replaces the base node; the base
    /// edge from the `File` node is masked, so the literal has one edge.
    #[test]
    fn test_replay_path_literal_moved_line_replaces_base_node_and_edge() {
        let (bytes, file, lit) = php_base();
        let graph = access(&bytes);
        let records = literals(vec![literal("/Lib.php", 2, RawEndpoint::File)]);
        let view = with_records(graph, &[input("src/main.php", Vec::new())], &[records]);

        let virt = only(virtual_of(&view, NodeKind::PathLiteral));
        assert_eq!(view.redirect(lit), Some(virt));
        let node = view.node(virt).unwrap();
        assert_eq!((node.start_line, node.name.as_str()), (3, "/Lib.php"));
        assert_eq!(
            node.uid,
            crate::uid::compute(NodeKind::PathLiteral, "src/main.php", None, "/Lib.php")
        );
        assert_eq!(
            rel_edges(graph, &view, RelType::UsesPathLiteral),
            [(file, virt, SINK.to_string())]
        );
    }

    /// Without records the record outputs stay as indexed: the base literal
    /// and its edge, at the old line. A records list of another length
    /// counts as none.
    #[test]
    fn test_replay_without_records_keeps_base_literal_as_indexed() {
        let (bytes, file, lit) = php_base();
        let graph = access(&bytes);
        let files = [input("src/main.php", Vec::new())];
        let two = [OverlayFileRecords::default(), OverlayFileRecords::default()];
        for view in [
            OverlayView::build(graph, &files).unwrap(),
            OverlayView::build_with_records(graph, &files, &two, &[]).unwrap(),
        ] {
            assert_eq!(view.redirect(lit), Some(lit));
            assert!(virtual_of(&view, NodeKind::PathLiteral).is_empty());
            assert_eq!(
                rel_edges(graph, &view, RelType::UsesPathLiteral),
                [(file, lit, SINK.to_string())]
            );
        }
    }

    /// An edit that empties the file removes its literal and the edge.
    #[test]
    fn test_replay_emptied_file_suppresses_base_literal_and_edge() {
        let (bytes, file, lit) = php_base();
        let graph = access(&bytes);
        let view = with_records(
            graph,
            &[input("src/main.php", Vec::new())],
            &[OverlayFileRecords::default()],
        );
        assert_eq!(view.redirect(lit), None);
        assert_eq!(view.redirect(file), Some(file));
        assert!(rel_edges(graph, &view, RelType::UsesPathLiteral).is_empty());
    }

    /// Two identical literals of one owner share a uid, and the index keeps
    /// both nodes: the base one pairs with the first, the second is new.
    #[test]
    fn test_replay_two_identical_literals_pair_first_and_add_second() {
        let (bytes, file, lit) = php_base();
        let graph = access(&bytes);
        let records = literals(vec![
            literal("/Lib.php", 3, RawEndpoint::File),
            literal("/Lib.php", 7, RawEndpoint::File),
        ]);
        let view = with_records(graph, &[input("src/main.php", Vec::new())], &[records]);

        let virt = virtual_of(&view, NodeKind::PathLiteral);
        assert_eq!(virt.len(), 2);
        assert_eq!(view.redirect(lit), Some(virt[0]));
        assert_eq!(
            view.node(virt[0]).unwrap().uid,
            view.node(virt[1]).unwrap().uid
        );
        assert_eq!(
            rel_edges(graph, &view, RelType::UsesPathLiteral),
            [
                (file, virt[0], SINK.to_string()),
                (file, virt[1], SINK.to_string())
            ]
        );
    }

    /// A literal inside a nested function of a new file binds the nested
    /// function. It has no owner, so the uid's owner segment is its lexical
    /// parent's name, as the index derives it (`path_literal_nodes`).
    #[test]
    fn test_replay_literal_in_nested_function_sources_nested_function() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/nest.ts",
            vec![func("outer", (1, 5)), func("inner", (2, 3))],
        )];
        let records = literals(vec![literal("./cfg.json", 2, RawEndpoint::Node(RawIdx(1)))]);
        let view = with_records(graph, &files, &[records]);

        let inner = view.base_len() + 1;
        assert_eq!(view.node(inner).unwrap().name, "inner");
        let virt = only(virtual_of(&view, NodeKind::PathLiteral));
        assert_eq!(
            view.node(virt).unwrap().uid,
            crate::uid::compute(
                NodeKind::PathLiteral,
                "src/nest.ts",
                Some("outer"),
                "./cfg.json"
            )
        );
        assert_eq!(
            rel_edges(graph, &view, RelType::UsesPathLiteral),
            [(inner, virt, SINK.to_string())]
        );
    }

    /// A table no `Class` declares gets no edge; a declared one gets the
    /// statement's verb as its reason.
    #[test]
    fn test_replay_sql_absent_table_no_edge_present_table_one_edge() {
        let mut fx = GraphFixture::new();
        let users = fx.node(NodeKind::Class, "src/schema.sql", "users");
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [input("src/api.py", vec![func("list_users", (1, 3))])];
        let records = OverlayFileRecords {
            sql_refs: vec![sql(
                &[("ghost", SqlVerb::Read), ("users", SqlVerb::Write)],
                RawEndpoint::Node(RawIdx(0)),
            )],
            ..OverlayFileRecords::default()
        };
        let view = with_records(graph, &files, &[records]);
        assert_eq!(
            rel_edges(graph, &view, RelType::QueriesTable),
            [(view.base_len(), users, "write".to_string())]
        );
    }

    /// The table is the first `Class` of its name in a fresh build's node
    /// order, files by path: a dirty file's declaration wins over a clean
    /// one at a later path and loses to one at an earlier path.
    #[test]
    fn test_replay_sql_table_first_declaration_by_path_wins() {
        let mut fx = GraphFixture::new();
        let early_users = fx.node(NodeKind::Class, "a/users.sql", "users");
        fx.node(NodeKind::Class, "z/orders.sql", "orders");
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [
            input("src/api.py", vec![func("report", (1, 3))]),
            input(
                "db/schema.sql",
                vec![
                    sym(NodeKind::Class, None, "users", (1, 1)),
                    sym(NodeKind::Class, None, "orders", (2, 2)),
                ],
            ),
        ];
        let records = [
            OverlayFileRecords {
                sql_refs: vec![sql(
                    &[("users", SqlVerb::Read), ("orders", SqlVerb::Read)],
                    RawEndpoint::Node(RawIdx(0)),
                )],
                ..OverlayFileRecords::default()
            },
            OverlayFileRecords::default(),
        ];
        let view = with_records(graph, &files, &records);
        let report = view.base_len();
        let dirty_orders = view.base_len() + 2;
        assert_eq!(view.node(dirty_orders).unwrap().name, "orders");
        assert_eq!(
            rel_edges(graph, &view, RelType::QueriesTable),
            [
                (report, early_users, "read".to_string()),
                (report, dirty_orders, "read".to_string())
            ]
        );
    }

    /// A dirty file that adds the only publisher of a topic gets the topic
    /// node and the `Publishes` edge; a whitespace-only topic is a topic as
    /// written; a dynamic topic gets nothing. The heuristic mirror edge to
    /// the clean subscriber is index-time and absent.
    #[test]
    fn test_replay_event_topic_new_publisher_gets_node_and_edge_no_mirror() {
        let mut fx = GraphFixture::new();
        let consumer = fx.func("src/consumer.ts", "consume");
        let subscribed = fx.node(NodeKind::EventTopic, "src/consumer.ts", "orders");
        fx.edge(consumer, subscribed, RelType::Subscribes);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [input("src/producer.ts", vec![func("publish", (1, 4))])];
        let records = OverlayFileRecords {
            event_topics: vec![
                topic(Some("orders"), 1),
                topic(Some(" "), 2),
                topic(None, 3),
            ],
            ..OverlayFileRecords::default()
        };
        let view = with_records(graph, &files, &[records]);

        let publish = view.base_len();
        let topics = virtual_of(&view, NodeKind::EventTopic);
        let names: Vec<&str> = topics
            .iter()
            .map(|&idx| view.node(idx).unwrap().name.as_str())
            .collect();
        assert_eq!(names, ["orders", " "]);
        let reason = EVENT_TOPIC_PUBLISHES_REASON.to_string();
        assert_eq!(
            rel_edges(graph, &view, RelType::Publishes),
            [
                (publish, topics[0], reason.clone()),
                (publish, topics[1], reason)
            ]
        );
        assert!(rel_edges(graph, &view, RelType::EventTopicMirror).is_empty());
        assert_eq!(view.redirect(subscribed), Some(subscribed));
    }

    /// A transaction scope pairs with the base scope of its uid, and its
    /// edge starts at the replayed method; the base edge is masked.
    #[test]
    fn test_replay_tx_scope_pairs_base_scope_and_rebuilds_edge() {
        let framework = FrameworkId::SpringTransactional;
        let name = format!("tx_scope:save#{}", framework.as_str());
        let mut fx = GraphFixture::new();
        let save = fx.method("src/S.java", "S", "save");
        let scope = fx.node(NodeKind::TransactionScope, "src/S.java", &name);
        fx.edge(save, scope, RelType::OpensTxScope);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [input(
            "src/S.java",
            vec![sym(NodeKind::Method, Some("S"), "save", (2, 4))],
        )];
        let records = OverlayFileRecords {
            tx_scopes: vec![RawTxScope::new(0, framework)],
            ..OverlayFileRecords::default()
        };
        let view = with_records(graph, &files, &[records]);

        let virt = only(virtual_of(&view, NodeKind::TransactionScope));
        assert_eq!(view.redirect(scope), Some(virt));
        assert_eq!(view.node(virt).unwrap().name, name);
        assert_eq!(
            rel_edges(graph, &view, RelType::OpensTxScope),
            [(view.base_len(), virt, TX_SCOPE_REASON.to_string())]
        );
    }

    /// A dirty file's schema field is rebuilt with its `HasProperty`, and
    /// the base field is suppressed rather than paired (module doc), so the
    /// field has one parent.
    #[test]
    fn test_replay_schema_field_rebuilt_base_field_suppressed_one_parent() {
        let mut fx = GraphFixture::new();
        let class = fx.node(NodeKind::Class, "m.py", "User");
        let field = fx.node_owned(NodeKind::SchemaField, "m.py", "User", "email");
        fx.edge_with(
            class,
            field,
            RelType::HasProperty,
            1.0,
            SCHEMA_FIELD_HAS_PROPERTY_REASON,
        );
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [input(
            "m.py",
            vec![sym(NodeKind::Class, None, "User", (1, 3))],
        )];
        let records = OverlayFileRecords {
            schema_fields: vec![RawSchemaField {
                name: "email".into(),
                type_class: SchemaType::String,
                owner_class: "User".into(),
                framework: FrameworkId::Pydantic,
                span: (1, 4, 1, 15),
                endpoint: Some(RawEndpoint::Node(RawIdx(0))),
            }],
            ..OverlayFileRecords::default()
        };
        let view = with_records(graph, &files, &[records]);

        let virt = only(virtual_of(&view, NodeKind::SchemaField));
        assert_eq!(view.redirect(field), None);
        let node = view.node(virt).unwrap();
        assert_eq!(node.uid, graph.nodes[field as usize].uid.to_native());
        assert_eq!(node.owner_class.as_deref(), Some("User"));
        assert_eq!(
            rel_edges(graph, &view, RelType::HasProperty),
            [(
                view.base_len(),
                virt,
                SCHEMA_FIELD_HAS_PROPERTY_REASON.to_string()
            )]
        );
    }

    /// Two refreshes of one session build the same view and the same cache
    /// bytes: the replay is a function of its inputs.
    #[test]
    fn test_replay_records_twice_same_view_and_cache_bytes() {
        let (bytes, _, _) = php_base();
        let graph = access(&bytes);
        let files = [input("src/main.php", vec![func("load", (1, 4))])];
        let records = [literals(vec![
            literal("/Lib.php", 2, RawEndpoint::Node(RawIdx(0))),
            literal("/Other.php", 3, RawEndpoint::File),
        ])];
        let first = with_records(graph, &files, &records);
        let second = with_records(graph, &files, &records);
        assert_eq!(first, second);
        assert_eq!(first.to_cache_bytes(), second.to_cache_bytes());
    }

    // ── class membership (F7b1) ─────────────────────────────────────────────

    use crate::analyzer::membership::{CLASS_MEMBERSHIP_IMPL_REASON, CLASS_MEMBERSHIP_REASON};

    fn member(source: u32, target: u32) -> (u32, u32, String) {
        (source, target, CLASS_MEMBERSHIP_REASON.to_string())
    }

    /// A base class `K` with members `a` and `gone`, each with its
    /// `HasMethod`, in `src/k.ts`: (bytes, K).
    fn class_base() -> (Vec<u8>, u32) {
        let mut fx = GraphFixture::new();
        let k = fx.node(NodeKind::Class, "src/k.ts", "K");
        let a = fx.method("src/k.ts", "K", "a");
        let gone = fx.method("src/k.ts", "K", "gone");
        for target in [a, gone] {
            fx.edge_with(k, target, RelType::HasMethod, 1.0, CLASS_MEMBERSHIP_REASON);
        }
        (fx.into_bytes(), k)
    }

    /// An edit that keeps `a`, removes `gone` and adds `fresh`: the dirty
    /// class has one `HasMethod` per member it now declares, the new one
    /// included, and the base edge to the kept member is masked, so `a` has
    /// one parent.
    #[test]
    fn test_replay_dirty_class_new_member_gets_has_method_and_base_edge_masked() {
        let (bytes, k) = class_base();
        let graph = access(&bytes);
        let files = [input(
            "src/k.ts",
            vec![
                sym(NodeKind::Class, None, "K", (1, 9)),
                sym(NodeKind::Method, Some("K"), "a", (2, 3)),
                sym(NodeKind::Method, Some("K"), "fresh", (5, 6)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();

        assert_eq!(view.redirect(k), Some(base));
        assert!(view.masks_base_edge(k, RelType::HasMethod));
        assert_eq!(
            rel_edges(graph, &view, RelType::HasMethod),
            [member(base, base + 1), member(base, base + 2)]
        );
    }

    /// Membership `HasProperty` shares its kind with a schema field's, so it
    /// is rebuilt only with the records: without them the base edge stays
    /// (redirected) and the new property gets none; with them each property
    /// has one edge and the view still round-trips its cache.
    #[test]
    fn test_replay_member_property_rebuilt_only_with_records() {
        let mut fx = GraphFixture::new();
        let k = fx.node(NodeKind::Class, "src/k.ts", "K");
        let p = fx.node_owned(NodeKind::Property, "src/k.ts", "K", "p");
        fx.edge_with(k, p, RelType::HasProperty, 1.0, CLASS_MEMBERSHIP_REASON);
        let bytes = fx.into_bytes();
        let graph = access(&bytes);
        let files = [input(
            "src/k.ts",
            vec![
                sym(NodeKind::Class, None, "K", (1, 5)),
                sym(NodeKind::Property, Some("K"), "p", (2, 2)),
                sym(NodeKind::Property, Some("K"), "q", (3, 3)),
            ],
        )];

        let without = OverlayView::build(graph, &files).unwrap();
        let base = without.base_len();
        assert!(!without.masks_base_edge(k, RelType::HasProperty));
        assert_eq!(
            rel_edges(graph, &without, RelType::HasProperty),
            [member(base, base + 1)]
        );

        let with = with_records(graph, &files, &[OverlayFileRecords::default()]);
        assert!(with.masks_base_edge(k, RelType::HasProperty));
        assert_eq!(
            rel_edges(graph, &with, RelType::HasProperty),
            [member(base, base + 1), member(base, base + 2)]
        );
    }

    /// An edit that empties the file leaves no membership edge.
    #[test]
    fn test_replay_emptied_class_file_has_no_membership_edge() {
        let (bytes, _) = class_base();
        let graph = access(&bytes);
        let view = OverlayView::build(graph, &[input("src/k.ts", Vec::new())]).unwrap();
        assert!(rel_edges(graph, &view, RelType::HasMethod).is_empty());
    }

    /// A removed file has no parse: its classes and their membership edges
    /// are gone.
    #[test]
    fn test_replay_removed_class_file_has_no_membership_edge() {
        let (bytes, _) = class_base();
        let graph = access(&bytes);
        let view = OverlayView::build_with_removed(graph, &[], &["src/k.ts".to_string()]).unwrap();
        assert!(rel_edges(graph, &view, RelType::HasMethod).is_empty());
    }

    /// Two refreshes of one session build the same membership edges and the
    /// same cache bytes.
    #[test]
    fn test_replay_membership_twice_same_view_and_cache_bytes() {
        let (bytes, _) = class_base();
        let graph = access(&bytes);
        let files = [input(
            "src/k.ts",
            vec![
                sym(NodeKind::Class, None, "K", (1, 9)),
                sym(NodeKind::Method, Some("K"), "fresh", (2, 3)),
            ],
        )];
        let records = [OverlayFileRecords::default()];
        let first = with_records(graph, &files, &records);
        let second = with_records(graph, &files, &records);
        assert_eq!(first, second);
        assert_eq!(first.to_cache_bytes(), second.to_cache_bytes());
        assert_eq!(rel_edges(graph, &first, RelType::HasMethod).len(), 1);
    }

    /// A Java constructor named like its class binds the constructor, never
    /// the class itself (`class_membership`'s self-loop rule).
    #[test]
    fn test_replay_member_named_like_class_binds_member_without_self_loop() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/Foo.java",
            vec![
                sym(NodeKind::Class, None, "Foo", (1, 10)),
                sym(NodeKind::Method, Some("Foo"), "Foo", (2, 4)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        assert_eq!(
            rel_edges(graph, &view, RelType::HasMethod),
            [member(base, base + 1)]
        );
    }

    /// A Rust `impl` method outside its struct's span binds through its
    /// owner, with the owner pass's reason.
    #[test]
    fn test_replay_impl_method_outside_struct_span_binds_by_owner() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/lib.rs",
            vec![
                sym(NodeKind::Struct, None, "Foo", (1, 1)),
                sym(NodeKind::Method, Some("Foo"), "new", (5, 7)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        assert_eq!(
            rel_edges(graph, &view, RelType::HasMethod),
            [(base, base + 1, CLASS_MEMBERSHIP_IMPL_REASON.to_string())]
        );
    }

    /// A class declared twice in one file: the second is a tombstone of
    /// the first, which owns the members of both, as the index's name
    /// lookup returns the first declaration.
    #[test]
    fn test_replay_class_declared_twice_members_bind_first_declaration() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/k.ts",
            vec![
                sym(NodeKind::Class, None, "K", (1, 3)),
                sym(NodeKind::Method, Some("K"), "a", (2, 2)),
                sym(NodeKind::Class, None, "K", (5, 7)),
                sym(NodeKind::Method, Some("K"), "b", (6, 6)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        assert_eq!(
            rel_edges(graph, &view, RelType::HasMethod),
            [member(base, base + 1), member(base, base + 3)]
        );
    }

    /// Two overloads of one method each get their own edge: a name lookup
    /// returns the first, so the later one binds its own slot.
    #[test]
    fn test_replay_overloaded_member_each_overload_gets_own_edge() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/K.java",
            vec![
                sym(NodeKind::Class, None, "K", (1, 9)),
                sym(NodeKind::Method, Some("K"), "save", (2, 3)),
                sym(NodeKind::Method, Some("K"), "save", (5, 6)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        assert!(view.node(base + 2).unwrap().later_overload);
        assert_eq!(
            rel_edges(graph, &view, RelType::HasMethod),
            [member(base, base + 1), member(base, base + 2)]
        );
    }

    /// A function nested in a TypeScript method is scoped: its callable
    /// declares it, so the class does not own it.
    #[test]
    fn test_replay_scoped_function_in_method_gets_no_membership() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let files = [input(
            "src/k.ts",
            vec![
                sym(NodeKind::Class, None, "K", (1, 10)),
                sym(NodeKind::Method, Some("K"), "m", (2, 8)),
                func("inner", (3, 4)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        assert_eq!(view.lexical_parent(base + 2), Some(base + 1));
        assert_eq!(
            rel_edges(graph, &view, RelType::HasMethod),
            [member(base, base + 1)]
        );
    }

    /// A C# member of `Conv<T>` beside `Conv` belongs to the arity twin,
    /// which no name lookup returns: the twin's own slot is the owner.
    #[test]
    fn test_replay_csharp_arity_twin_owns_its_member() {
        let bytes = clean_base();
        let graph = access(&bytes);
        let mut generic = sym(NodeKind::Class, None, "Conv", (5, 7));
        generic.generic_arity = 1;
        let files = [input(
            "src/Conv.cs",
            vec![
                sym(NodeKind::Class, None, "Conv", (1, 3)),
                generic,
                sym(NodeKind::Method, Some("Conv"), "Run", (6, 6)),
            ],
        )];
        let view = OverlayView::build(graph, &files).unwrap();
        let base = view.base_len();
        assert!(view.node(base + 1).unwrap().later_overload, "a twin");
        assert_eq!(
            rel_edges(graph, &view, RelType::HasMethod),
            [member(base + 1, base + 2)]
        );
    }
}
